------------------------- MODULE SubscribeAccounting -------------------------
(***************************************************************************)
(* How one lite-07 subscription accounts for each group, between the       *)
(* publisher's `TrackRun` (rs/moq-net/src/lite/publisher.rs) and the       *)
(* subscriber's `TrackServe` (rs/moq-net/src/lite/subscriber.rs) with its  *)
(* `Tail` (rs/moq-net/src/tail.rs) and its copy of the track.              *)
(*                                                                         *)
(* The draft's promise: for every sequence in range the publisher sends a  *)
(* Group Stream or names it in SUBSCRIBE_SKIP or SUBSCRIBE_DROP. Every     *)
(* drop is final on its subscription, a stale (OLD) one included: a        *)
(* SUBSCRIBE_UPDATE reopens nothing, so updates are left out. A skip still *)
(* goes out over a drop of a group never served. Drops and skips travel in *)
(* order on the Subscribe Stream; each Group Stream travels on its own,    *)
(* unordered against them, and a reset can lose its header.                *)
(*                                                                         *)
(* A Group Stream whose group expires stale resets with OLD and is named  *)
(* in a stale drop, whether it never opened, is serving, or already sent   *)
(* its FIN (after a GROUP_END) and awaits the acknowledgement: stale bytes *)
(* must not hold the send window. A subscriber that read it whole first    *)
(* keeps the group. A stream reset while serving, evicted or preempted     *)
(* with TOO_FAR_BEHIND for a group that outranks it, is named in a final   *)
(* drop.                                                                   *)
(*                                                                         *)
(* Each GROUP carries the previous group's start, passing over the         *)
(* skipped starts directly below it as a SUBSCRIBE_SKIP would. The         *)
(* publisher holds such a skip back for the GROUP of a group it still      *)
(* owes, and sends it after all, ahead of the drop, if it gives that group *)
(* up instead. A drop of a group whose stream opened goes out after a      *)
(* skip of what its GROUP passed over, since the GROUP may never arrive.   *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event. The track     *)
(* cursor's verdicts (hand a group out, give it up stale or final, skip    *)
(* it) are free choices, standing in for the cache, the clock, and the     *)
(* routes. So are when a stream fails, standing in for stream credit,      *)
(* expiry, eviction, and preemption, and whether the cache holds the group *)
(* above a skip, which decides whether the skip is held back for its       *)
(* GROUP. Under `Pressure`, so is when the pool's debt folds the           *)
(* publisher's record of the group.                                        *)
(*                                                                         *)
(* Scope: the subscription is already answered (SUBSCRIBE_OK) and covers   *)
(* every group; ranges, starts, fills, frames, and the tail's grace are    *)
(* left out. Group sequences start at 1 in `SubscribeAccounting.cfg`: a    *)
(* GROUP passes over a run only above start 0, the previous start it then  *)
(* names. GROUP_END takeover is left out.                                  *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,      \* group sequences in range
    Pressure     \* the cache pool may hand the publisher's reader debt, so its record folds

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err` ("old" or "evicted")
\*   fin: the FIN went out before any reset, so every byte may arrive first
\*   sub: "none" (no header yet, or lost with a reset, or never sent), "header"
\*        (filling the copy), "fin", or "reset"
\*   passes: the skipped groups its GROUP's Previous Start passes over
NewStream(g, passes) ==
    [g |-> g, pub |-> "open", fin |-> FALSE, err |-> "", sub |-> "none", passes |-> passes]

\* A drop names one group; a skip names a run of them.
DropMsg(g, err) == [type |-> "drop", gs |-> {g}, err |-> err]
SkipMsg(gs) == [type |-> "skip", gs |-> gs, err |-> ""]

VARIABLES
    \* The publisher's track cursor: its verdict on each group.
    cur,        \* "pending", "handed", "stale", "final", or "skip"
    late,       \* the cursor handed the group out once more
    \* The publisher's `TrackRun` and its reader's record.
    served,     \* `Record::told`: sequences served or given up
    dropped,    \* the part of `served` named in a drop, never served (`Report::Dropped`)
    outq,       \* `GroupOutcomes::dropped`: failed streams not yet named, each with the
                \* skipped groups its GROUP passed over
    held,       \* `TrackRun::elided`: per group, the skipped run held back for its GROUP
    folded,     \* the record folded the group into its floor
    ndrop,      \* history: drops naming the group
    nskip,      \* history: skips naming the group on their own
    nahead,     \* history: skips naming the group again, ahead of a drop of the group above
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    \* The subscriber's `Tail` and copy.
    acct,       \* the tail's ledger: groups accounted for
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached      \* the copy's group: "none", "open", "whole", "aborted", "evicted"

names == <<ndrop, nskip, nahead>>
pubVars == <<cur, late, served, dropped, outq, held, folded, names>>
subVars == <<acct, rec, cached>>
vars == <<pubVars, streams, resp, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ served = {} /\ dropped = {} /\ outq = <<>> /\ folded = {}
    /\ held = [g \in Groups |-> {}]
    /\ ndrop = [g \in Groups |-> 0]
    /\ nskip = [g \in Groups |-> 0]
    /\ nahead = [g \in Groups |-> 0]
    /\ streams = <<>> /\ resp = <<>>
    /\ acct = {}
    /\ rec = [g \in Groups |-> "none"]
    /\ cached = [g \in Groups |-> "none"]

-----------------------------------------------------------------------------
(* Publisher                                                               *)
-----------------------------------------------------------------------------

\* The skipped run directly below g, which g's GROUP passes over (`previous_start`): its
\* Previous Start names the start below the run, so the run starts above 0.
SkippedBelow(g) ==
    LET below == {x \in Groups : x < g /\ \A y \in x..g - 1 : y \in Groups /\ cur[y] = "skip"}
    IN IF 0 \in below THEN {} ELSE below

\* `serve`: open a Group Stream for g, which `mark_served` records. Its GROUP passes over
\* the skipped run below it and the one held back for it, which the cache may have forgotten.
Serve(g) ==
    /\ streams' = Append(streams, NewStream(g, SkippedBelow(g) \cup held[g]))
    /\ served' = served \cup {g}
    /\ held' = [held EXCEPT ![g] = {}]

\* `poll_track` takes `Recv::Group`: the cursor hands g out. Once more, it may hand out a
\* group it gave up: one a route the front switched to delivers after a stale verdict, or
\* one re-created after its drop. It never hands out a sequence twice. A sequence already
\* served or given up is not served (`Record::told`). `poll_step` names failed streams first.
CursorHand(g) ==
    /\ outq = <<>>
    /\ g \notin folded
    /\ \/ cur[g] = "pending" /\ UNCHANGED late
       \/ cur[g] \in {"stale", "final"} /\ ~late[g] /\ late' = [late EXCEPT ![g] = TRUE]
    /\ cur' = [cur EXCEPT ![g] = "handed"]
    /\ IF g \in served
       THEN UNCHANGED <<served, streams, held>>
       ELSE Serve(g)
    /\ UNCHANGED <<dropped, outq, folded, names, resp, subVars>>

\* Count each group of `gs` named once more in `n`.
Named(n, gs) == [x \in Groups |-> IF x \in gs THEN n[x] + 1 ELSE n[x]]

\* `poll_forward_drop`: the cursor gives g up stale ("old"), final ("evicted"), or
\* declares it skipped, after `poll_step` named failed streams. A later verdict may follow
\* an earlier one. `forward_range` sends it only where the record has told nothing, or, for
\* a skip, only a drop (`Report::blocks`). A run held back for g's GROUP goes out ahead of
\* g's drop, or joins g's skip. That skip is held back in turn for the GROUP of the group
\* above, if the subscription still owes it (`elides`): `hold` stands in for whether the
\* cache holds that group.
CursorGiveUp(g, kind, hold) ==
    /\ outq = <<>>
    /\ \/ cur[g] = "pending"
       \/ cur[g] = "stale" /\ kind \in {"evicted", "skip"}
       \/ cur[g] = "final" /\ kind = "skip"
    /\ cur' = [cur EXCEPT ![g] = IF kind = "old" THEN "stale" ELSE IF kind = "skip" THEN "skip" ELSE "final"]
    /\ LET named == g \notin served \/ (kind = "skip" /\ g \in dropped)
           run == held[g] \cup {g}
           above == g + 1
           elides == kind = "skip" /\ above \in Groups /\ above \notin served /\ 0 \notin run
       IN /\ hold => named /\ elides
          /\ served' = served \cup {g}
          /\ dropped' = IF ~named THEN dropped
                        ELSE IF kind = "skip" THEN dropped \ {g} ELSE dropped \cup {g}
          /\ held' = CASE ~named -> held
                        [] hold -> [held EXCEPT ![g] = {}, ![above] = @ \cup run]
                        [] OTHER -> [held EXCEPT ![g] = {}]
          /\ resp' = CASE ~named \/ hold -> resp
                        [] kind = "skip" -> Append(resp, SkipMsg(run))
                        [] held[g] # {} -> resp \o <<SkipMsg(held[g]), DropMsg(g, kind)>>
                        [] OTHER -> Append(resp, DropMsg(g, kind))
          /\ ndrop' = IF named /\ kind # "skip" THEN [ndrop EXCEPT ![g] = @ + 1] ELSE ndrop
          /\ nskip' = CASE ~named \/ hold -> nskip
                        [] kind = "skip" -> Named(nskip, run)
                        [] OTHER -> Named(nskip, held[g])
    /\ UNCHANGED <<late, outq, folded, nahead, streams, subVars>>

\* A serving stream FINs after its last frame (`GroupState::Closed`), after a GROUP_END
\* if its group finished. Nothing names the group unless the stream resets later.
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams' = [streams EXCEPT ![i].pub = "fin", ![i].fin = TRUE]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* A stream fails and resets. It expires stale ("old", `poll_given_up`): before it opened,
\* waiting for stream credit (`give_way`, `shed`), so nothing of it reached the wire; while
\* serving; or after its FIN, before the acknowledgement (`GroupState::Closed`). Or it is
\* reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks
\* it, both final ("evicted"). `settle_failed` queues the outcome, with what its GROUP
\* passed over.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err]
    /\ outq' = Append(outq, <<streams[i].g, err, streams[i].passes>>)
    /\ UNCHANGED <<cur, late, served, dropped, held, folded, names, resp, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub = "fin"
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* `poll_step` names each failed stream in a drop with its error, final like any other:
\* the group was served, so nothing serves or names it again. A skip of what its GROUP
\* passed over goes first, since the GROUP may never have arrived, even where a skip
\* already named it.
PubOutcomes ==
    /\ outq # <<>>
    /\ outq' = Tail(outq)
    /\ LET g == Head(outq)[1]
           drop == DropMsg(g, Head(outq)[2])
           passes == Head(outq)[3]
       IN /\ resp' = IF passes = {} THEN Append(resp, drop) ELSE resp \o <<SkipMsg(passes), drop>>
          /\ ndrop' = [ndrop EXCEPT ![g] = @ + 1]
          /\ nahead' = Named(nahead, passes)
    /\ UNCHANGED <<cur, late, served, dropped, held, folded, nskip, streams, subVars>>

\* The pool hands the reader debt and its record folds the group into its floor: a group
\* the cursor reached a verdict on, once no failed stream of it is still to name. Every
\* report is final, so the fold forgets which: a skip no longer goes out over a drop.
PubFold(g) ==
    /\ Pressure
    /\ g \notin folded
    /\ cur[g] # "pending"
    /\ \A i \in 1..Len(outq) : outq[i][1] # g
    /\ folded' = folded \cup {g}
    /\ dropped' = dropped \ {g}
    /\ UNCHANGED <<cur, late, served, outq, held, names, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* The copy's record `r` once a skip names `gs`: a group that arrived keeps it
\* (`skip_received`).
SkipRec(r, gs) == [x \in Groups |-> IF x \in gs /\ cached[x] \notin {"open", "whole"} THEN "skip" ELSE r[x]]

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group, and the stream fills the copy, replacing a stale record of it.
\* Its GROUP's Previous Start skips what it passes over, as a SUBSCRIBE_SKIP would.
SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].pub \in {"open", "fin", "reset"}
    /\ LET g == streams[i].g
           passes == streams[i].passes
           unstale == [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
       IN /\ acct' = acct \cup {g} \cup passes
          /\ streams' = [streams EXCEPT ![i].sub = "header"]
          /\ cached' = [cached EXCEPT ![g] = "open"]
          /\ rec' = SkipRec(unstale, passes)
    /\ UNCHANGED <<pubVars, resp>>

\* The stream filling the copy is read to its FIN, the group whole after a GROUP_END or
\* closed without one, which the copy keeps alike ("whole"). A reset sent after the FIN
\* may arrive too late to cut it.
SubFinAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].fin
    /\ streams' = [streams EXCEPT ![i].sub = "fin"]
    /\ cached' = [cached EXCEPT ![streams[i].g] = "whole"]
    /\ UNCHANGED <<pubVars, resp, acct, rec>>

\* The reset of the stream filling the copy arrives and ends the group's readers with its
\* error. The header already accounted for the group, and the drop naming it follows.
SubResetAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].pub = "reset"
    /\ streams' = [streams EXCEPT ![i].sub = "reset"]
    /\ cached' = [cached EXCEPT ![streams[i].g] = "aborted"]
    /\ UNCHANGED <<pubVars, resp, acct, rec>>

\* The copy's cache evicts a whole group.
Evict(g) ==
    /\ cached[g] = "whole"
    /\ cached' = [cached EXCEPT ![g] = "evicted"]
    /\ UNCHANGED <<pubVars, streams, resp, acct, rec>>

\* `apply_response` for SUBSCRIBE_DROP: the tail accounts for the group. A stale drop
\* records it in the copy unless the copy holds it (`give_up` keeps an open group), and a
\* final drop records it and ends its readers unless the copy holds it whole (`give_up`
\* keeps a group a finished copy serves, the live one or a fetched head). A fetched head
\* still filling, which this model leaves out, holds the drop in its slot until it
\* finishes ("whole" here) or dies, when the drop is the slot's cut (`Slot::verdict`):
\* never lost, never beside a group.
OnDrop(m) ==
    LET g == CHOOSE g \in m.gs : TRUE
    IN /\ acct' = acct \cup {g}
       /\ IF m.err = "old"
          THEN /\ rec' = IF cached[g] \notin {"open", "whole"} /\ rec[g] # "skip"
                         THEN [rec EXCEPT ![g] = "stale"] ELSE rec
               /\ UNCHANGED cached
          ELSE /\ rec' = IF rec[g] = "skip" \/ cached[g] = "whole" THEN rec ELSE [rec EXCEPT ![g] = "final"]
               /\ cached' = [cached EXCEPT ![g] = IF @ = "open" THEN "aborted" ELSE @]

\* `apply_response` for SUBSCRIBE_SKIP: final, overriding any drop, but a group that
\* arrived keeps it (`skip_received`).
OnSkip(m) ==
    /\ acct' = acct \cup m.gs
    /\ rec' = SkipRec(rec, m.gs)
    /\ UNCHANGED cached

SubRecv ==
    /\ resp # <<>>
    /\ resp' = Tail(resp)
    /\ LET m == Head(resp)
       IN IF m.type = "drop" THEN OnDrop(m) ELSE OnSkip(m)
    /\ UNCHANGED <<pubVars, streams>>

-----------------------------------------------------------------------------

\* A stream is done once both sides stopped: acknowledged, or reset with the header
\* read to the reset or to the FIN before it, lost, or never sent.
StreamDone(s) ==
    \/ s.pub = "acked"
    \/ s.pub = "reset" /\ s.sub \in {"none", "reset", "fin"}

\* Nothing in flight and nothing left for the publisher to do.
Quiet ==
    /\ resp = <<>> /\ outq = <<>>
    /\ \A g \in Groups : cur[g] # "pending"
    /\ \A i \in 1..Len(streams) : StreamDone(streams[i])

\* The end. Any other state where nothing can happen is a deadlock TLC reports.
Done ==
    /\ Quiet
    /\ UNCHANGED vars

\* Each action over the streams, named so a trace says which one ran.
StreamFin == \E i \in 1..Len(streams) : StreamFinAt(i)
StreamFail == \E i \in 1..Len(streams), err \in {"old", "evicted"} : StreamFailAt(i, err)
StreamAck == \E i \in 1..Len(streams) : StreamAckAt(i)
SubHeader == \E i \in 1..Len(streams) : SubHeaderAt(i)
SubFin == \E i \in 1..Len(streams) : SubFinAt(i)
SubReset == \E i \in 1..Len(streams) : SubResetAt(i)

Next ==
    \/ \E g \in Groups : CursorHand(g)
    \/ \E g \in Groups, kind \in {"old", "evicted", "skip"}, hold \in BOOLEAN : CursorGiveUp(g, kind, hold)
    \/ StreamFin
    \/ StreamFail
    \/ StreamAck
    \/ PubOutcomes
    \/ \E g \in Groups : PubFold(g)
    \/ SubHeader
    \/ SubFin
    \/ SubReset
    \/ \E g \in Groups : Evict(g)
    \/ SubRecv
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules                                                                   *)
-----------------------------------------------------------------------------

\* Once quiet, the tail accounts for every group: none is waited for until the grace.
QuietIsSettled == Quiet => acct = Groups

\* The publisher opens at most one Group Stream for a group, so the subscriber never reads
\* two at once and no group's bytes go out twice.
ServedOnce ==
    \A i, j \in 1..Len(streams) : i # j => streams[i].g # streams[j].g

\* Every name is final: the publisher names a group in at most one drop, and in at most
\* one skip, which goes out over a drop of a group it never served and over nothing else.
\* The one exception is a skip of a run a GROUP passed over, repeated once ahead of that
\* group's drop. Neither ever names a group that has a Group Stream.
NamedOnce ==
    \A g \in Groups :
        /\ ndrop[g] <= 1
        /\ nskip[g] <= 1
        /\ nahead[g] <= 1
        /\ nskip[g] + nahead[g] > 0 => \A i \in 1..Len(streams) : streams[i].g # g

=============================================================================
