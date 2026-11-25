------------------------- MODULE SubscribeAccounting -------------------------
(***************************************************************************)
(* How one lite-07 subscription accounts for each group, between the       *)
(* publisher's `TrackRun` (rs/moq-net/src/lite/publisher.rs) and the       *)
(* subscriber's `TrackServe` (rs/moq-net/src/lite/subscriber.rs) with its  *)
(* `Tail` (rs/moq-net/src/tail.rs) and its copy of the track.              *)
(*                                                                         *)
(* The draft's promise: for every start in range the publisher sends a     *)
(* Group Stream, accounts for it with a GROUP_END, or names it in          *)
(* SUBSCRIBE_SKIP or SUBSCRIBE_DROP. Every drop is final on its            *)
(* subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens    *)
(* nothing, so updates are left out. A skip still goes out over a drop of  *)
(* a group never served. Drops and skips travel in order on the Subscribe  *)
(* Stream; each Group Stream travels on its own, unordered against them,   *)
(* and a reset can lose its header.                                        *)
(*                                                                         *)
(* A Group Stream whose group expires stale resets with OLD and is named  *)
(* in a stale drop, whether it never opened, is serving, or already sent   *)
(* its FIN (after a GROUP_END) and awaits the acknowledgement: stale bytes *)
(* must not hold the send window. A subscriber that read it whole first    *)
(* keeps the group. A stream reset while serving, evicted or preempted     *)
(* with TOO_FAR_BEHIND for a group that outranks it, is named in a final   *)
(* drop.                                                                   *)
(*                                                                         *)
(* A whole group's span runs from its start to its End, and no group      *)
(* starts strictly inside it. Its GROUP_END accounts for the starts        *)
(* strictly inside the span, which the publisher then records as told, and *)
(* a drop of a group the publisher holds whole names the span. Every skip  *)
(* goes out as a SUBSCRIBE_SKIP. The track records a start inside a whole  *)
(* group's span skipped (`append_group`, or a copy's GROUP_END), and the   *)
(* cursor skips it where nothing told it yet, as when the group's stream   *)
(* was dropped before the group finished.                                  *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event. The track     *)
(* cursor's verdicts (hand a group out, give it up stale or final, skip    *)
(* it) are free choices, standing in for the cache, the clock, and the     *)
(* routes. So are when a stream fails, standing in for stream credit,      *)
(* expiry, eviction, and preemption, and when a group finishes whole and   *)
(* where it ends. Under `Pressure`, so is when the pool's debt folds the   *)
(* publisher's record of the group, which also stands for the fold a       *)
(* record runs as it doubles (`Fold::Final`): that one folds only skips    *)
(* and settled reports, a subset of what this may.                         *)
(*                                                                         *)
(* Scope: the subscription is already started (SUBSCRIBE_START) and covers *)
(* every start in `Groups`; ranges, fills, frames, and the tail's grace    *)
(* are left out. An End is at most one past the highest start, so a span   *)
(* may run past the range. GROUP_END takeover is left out.                 *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,      \* starts in range
    Pressure     \* the cache pool may hand the publisher's reader debt, so its record folds

\* The highest End a group may finish at: one past the highest start.
MaxEnd == 1 + CHOOSE m \in Groups : \A x \in Groups : x <= m

\* The starts strictly inside the span of a group starting at g and ending at e.
Inner(g, e) == {x \in Groups : g < x /\ x < e}

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err` ("old" or "evicted")
\*   fin: the FIN went out before any reset, so every byte may arrive first
\*   ended: the FIN followed a GROUP_END, accounting for the starts in `inner`
\*   sub: "none" (no header yet, or lost with a reset, or never sent), "header"
\*        (filling the copy), "fin", or "reset"
NewStream(g) ==
    [g |-> g, pub |-> "open", fin |-> FALSE, ended |-> FALSE, inner |-> {}, err |-> "", sub |-> "none"]

\* A drop names a group, with its span once whole; a skip names one start.
DropMsg(gs, err) == [type |-> "drop", gs |-> gs, err |-> err]
SkipMsg(gs) == [type |-> "skip", gs |-> gs, err |-> ""]

VARIABLES
    \* The publisher's track cursor: its verdict on each start.
    cur,        \* "pending", "handed", "stale", "final", "skip", or "inner" (inside a
                \* whole group's span)
    late,       \* the cursor handed the group out once more
    ends,       \* the group's End once it finished whole, else 0 (`finish_at`)
    \* The publisher's `TrackRun` and its reader's record.
    served,     \* `Record::told`: starts served, accounted, or given up
    dropped,    \* the part of `served` named in a drop, never served (`Report::Dropped`)
    outq,       \* `GroupOutcomes::dropped`: failed streams not yet named
    folded,     \* the record folded the group into its floor
    ndrop,      \* history: drops naming the group, not counting the span above it
    nskip,      \* history: skips naming the start
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    \* The subscriber's `Tail` and copy.
    acct,       \* the tail's ledger: starts accounted for
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached      \* the copy's group: "none", "open", "whole", "aborted", "evicted"

names == <<ndrop, nskip>>
pubVars == <<cur, late, ends, served, dropped, outq, folded, names>>
subVars == <<acct, rec, cached>>
vars == <<pubVars, streams, resp, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ ends = [g \in Groups |-> 0]
    /\ served = {} /\ dropped = {} /\ outq = <<>> /\ folded = {}
    /\ ndrop = [g \in Groups |-> 0]
    /\ nskip = [g \in Groups |-> 0]
    /\ streams = <<>> /\ resp = <<>>
    /\ acct = {}
    /\ rec = [g \in Groups |-> "none"]
    /\ cached = [g \in Groups |-> "none"]

-----------------------------------------------------------------------------
(* Publisher                                                               *)
-----------------------------------------------------------------------------

\* The starts a drop of g names: its span once it finished whole, else g alone.
Span(g) == {g} \cup (IF ends[g] = 0 THEN {} ELSE Inner(g, ends[g]))

\* Count each start of `gs` named once more in `n`.
Named(n, gs) == [x \in Groups |-> IF x \in gs THEN n[x] + 1 ELSE n[x]]

\* The group at g finishes whole at End e (`finish_at`). No group starts strictly inside
\* its span: those starts hold no group and none is handed or given up there, though one
\* may already be skipped, as when a later group was appended first. A group closed
\* without a GROUP_END (a FIN alone) never finishes.
Finish(g, e) ==
    /\ ends[g] = 0
    /\ cur[g] \notin {"skip", "inner"}
    /\ \A i \in 1..Len(streams) : streams[i].g = g => ~(streams[i].fin /\ ~streams[i].ended)
    /\ \A x \in Inner(g, e) : cur[x] \in {"pending", "skip"}
    /\ ends' = [ends EXCEPT ![g] = e]
    /\ cur' = [x \in Groups |-> IF x \in Inner(g, e) /\ cur[x] = "pending" THEN "inner" ELSE cur[x]]
    /\ UNCHANGED <<late, served, dropped, outq, folded, names, streams, resp, subVars>>

\* `poll_track` takes `Recv::Group`: the cursor hands g out. Once more, it may hand out a
\* group it gave up: one a route the front switched to delivers after a stale verdict, or
\* one re-created after its drop. It never hands out a start twice. A start already
\* served or given up is not served (`Record::told`); `serve` opens a Group Stream for
\* the rest, which `mark_served` records. `poll_step` names failed streams first.
CursorHand(g) ==
    /\ outq = <<>>
    /\ g \notin folded
    /\ \/ cur[g] = "pending" /\ UNCHANGED late
       \/ cur[g] \in {"stale", "final"} /\ ~late[g] /\ late' = [late EXCEPT ![g] = TRUE]
    /\ cur' = [cur EXCEPT ![g] = "handed"]
    /\ IF g \in served
       THEN UNCHANGED <<served, streams>>
       ELSE /\ streams' = Append(streams, NewStream(g))
            /\ served' = served \cup {g}
    /\ UNCHANGED <<ends, dropped, outq, folded, names, resp, subVars>>

\* `poll_forward_drop`: the cursor gives g up stale ("old"), final ("evicted"), or
\* declares it skipped, after `poll_step` named failed streams. A later verdict may follow
\* an earlier one. A group that finished whole exists, so it is never skipped.
\* `forward_range` sends it only where the record has told nothing, or, for a skip, only a
\* drop (`Report::blocks`). A drop of a group held whole names its span.
CursorGiveUp(g, kind) ==
    /\ outq = <<>>
    /\ \/ cur[g] = "pending"
       \/ cur[g] = "stale" /\ kind \in {"evicted", "skip"}
       \/ cur[g] = "final" /\ kind = "skip"
    /\ kind = "skip" => ends[g] = 0
    /\ cur' = [cur EXCEPT ![g] = IF kind = "old" THEN "stale" ELSE IF kind = "skip" THEN "skip" ELSE "final"]
    /\ LET named == g \notin served \/ (kind = "skip" /\ g \in dropped)
           gs == IF kind = "skip" THEN {g} ELSE Span(g)
       IN /\ served' = served \cup (IF named THEN gs ELSE {g})
          /\ dropped' = IF ~named THEN dropped
                        ELSE IF kind = "skip" THEN dropped \ {g} ELSE dropped \cup {g}
          /\ resp' = CASE ~named -> resp
                        [] kind = "skip" -> Append(resp, SkipMsg(gs))
                        [] OTHER -> Append(resp, DropMsg(gs, kind))
          /\ ndrop' = IF named /\ kind # "skip" THEN Named(ndrop, {g}) ELSE ndrop
          /\ nskip' = IF named /\ kind = "skip" THEN Named(nskip, gs) ELSE nskip
    /\ UNCHANGED <<late, ends, outq, folded, streams, subVars>>

\* The cursor reaches a start inside a whole group's span, which the track records
\* skipped (`append_group`, or a copy's GROUP_END), and skips it where nothing told it yet.
CursorHole(x) ==
    /\ outq = <<>>
    /\ cur[x] = "inner"
    /\ x \notin served
    /\ served' = served \cup {x}
    /\ resp' = Append(resp, SkipMsg({x}))
    /\ nskip' = Named(nskip, {x})
    /\ UNCHANGED <<cur, late, ends, dropped, outq, folded, ndrop, streams, subVars>>

\* A serving stream FINs after its last frame (`GroupState::Closed`). Once its group
\* finished whole, a GROUP_END goes first, and `buffer_group_end` records the starts
\* strictly inside the span as told. Otherwise the group is closed. Nothing names the
\* group unless the stream resets later.
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ LET g == streams[i].g
           ended == ends[g] # 0
           inner == IF ended THEN Inner(g, ends[g]) ELSE {}
       IN /\ streams' = [streams EXCEPT ![i].pub = "fin", ![i].fin = TRUE,
                                        ![i].ended = ended, ![i].inner = inner]
          /\ served' = served \cup inner
    /\ UNCHANGED <<cur, late, ends, dropped, outq, folded, names, resp, subVars>>

\* A stream fails and resets. It expires stale ("old", `poll_given_up`): before it opened,
\* waiting for stream credit (`give_way`, `shed`), so nothing of it reached the wire; while
\* serving; or after its FIN, before the acknowledgement (`GroupState::Closed`). Or it is
\* reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks
\* it, both final ("evicted"). `settle_failed` queues the outcome.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err]
    /\ outq' = Append(outq, <<streams[i].g, err>>)
    /\ UNCHANGED <<cur, late, ends, served, dropped, folded, names, resp, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub = "fin"
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* `poll_step` names each failed stream in a drop with its error, final like any other:
\* the group was served, so nothing serves or names it again. A group held whole is named
\* with its span, since the reset can lose its GROUP_END.
PubOutcomes ==
    /\ outq # <<>>
    /\ outq' = Tail(outq)
    /\ LET g == Head(outq)[1]
           gs == Span(g)
       IN /\ resp' = Append(resp, DropMsg(gs, Head(outq)[2]))
          /\ served' = served \cup gs
          /\ ndrop' = Named(ndrop, {g})
    /\ UNCHANGED <<cur, late, ends, dropped, folded, nskip, streams, subVars>>

\* The pool hands the reader debt and its record folds the start into its floor: one the
\* cursor reached a verdict on, once no failed stream of it is still to name. Every
\* report is final, so the fold forgets which: a skip no longer goes out over a drop.
PubFold(g) ==
    /\ Pressure
    /\ g \notin folded
    /\ cur[g] # "pending"
    /\ \A i \in 1..Len(outq) : outq[i][1] # g
    /\ folded' = folded \cup {g}
    /\ dropped' = dropped \ {g}
    /\ UNCHANGED <<cur, late, ends, served, outq, names, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group, and the stream fills the copy, replacing a stale record of it.
SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].pub \in {"open", "fin", "reset"}
    /\ LET g == streams[i].g
       IN /\ acct' = acct \cup {g}
          /\ streams' = [streams EXCEPT ![i].sub = "header"]
          /\ cached' = [cached EXCEPT ![g] = "open"]
          /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
    /\ UNCHANGED <<pubVars, resp>>

\* The stream filling the copy is read to its FIN, the group whole after a GROUP_END or
\* closed without one, which the copy keeps alike ("whole"). A GROUP_END accounts for the
\* starts strictly inside the span, which the copy derives from the whole group and
\* records nothing else for. A reset sent after the FIN may arrive too late to cut it.
SubFinAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].fin
    /\ streams' = [streams EXCEPT ![i].sub = "fin"]
    /\ acct' = acct \cup streams[i].inner
    /\ cached' = [cached EXCEPT ![streams[i].g] = "whole"]
    /\ UNCHANGED <<pubVars, resp, rec>>

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

\* `apply_response` for SUBSCRIBE_DROP: the tail accounts for each start it names. A
\* stale drop records it in the copy unless the copy holds it (`give_up` keeps an open
\* group), and a final drop records it and ends its readers unless the copy holds it
\* whole (`give_up` keeps a group a finished copy serves, the live one or a fetched
\* head). A fetched head still filling, which this model leaves out, holds the drop in
\* its slot until it finishes ("whole" here) or dies, when the drop is the slot's cut
\* (`Slot::verdict`): never lost, never beside a group. A drop after a skip of the same
\* start changes nothing.
OnDrop(m) ==
    /\ acct' = acct \cup m.gs
    /\ IF m.err = "old"
       THEN /\ rec' = [x \in Groups |->
                        IF x \in m.gs /\ cached[x] \notin {"open", "whole"} /\ rec[x] # "skip"
                        THEN "stale" ELSE rec[x]]
            /\ UNCHANGED cached
       ELSE /\ rec' = [x \in Groups |->
                        IF x \in m.gs /\ rec[x] # "skip" /\ cached[x] # "whole"
                        THEN "final" ELSE rec[x]]
            /\ cached' = [x \in Groups |-> IF x \in m.gs /\ cached[x] = "open" THEN "aborted" ELSE cached[x]]

\* `apply_response` for SUBSCRIBE_SKIP: final, overriding any drop, but a group that
\* arrived keeps it (`skip_received`).
OnSkip(m) ==
    /\ acct' = acct \cup m.gs
    /\ rec' = [x \in Groups |-> IF x \in m.gs /\ cached[x] \notin {"open", "whole"} THEN "skip" ELSE rec[x]]
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

\* Nothing in flight and nothing left for the publisher to do: every start has a verdict,
\* and every start inside a whole group's span is told.
Quiet ==
    /\ resp = <<>> /\ outq = <<>>
    /\ \A g \in Groups : cur[g] # "pending" /\ (cur[g] = "inner" => g \in served)
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
    \/ \E g \in Groups, e \in 1..MaxEnd : g < e /\ Finish(g, e)
    \/ \E g \in Groups : CursorHand(g)
    \/ \E g \in Groups, kind \in {"old", "evicted", "skip"} : CursorGiveUp(g, kind)
    \/ \E g \in Groups : CursorHole(g)
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

\* Once quiet, the tail accounts for every start: none is waited for until the grace.
QuietIsSettled == Quiet => acct = Groups

\* The publisher opens at most one Group Stream for a group, so the subscriber never reads
\* two at once and no group's bytes go out twice.
ServedOnce ==
    \A i, j \in 1..Len(streams) : i # j => streams[i].g # streams[j].g

\* Every name is final: the publisher names a group in at most one drop, and a start in at
\* most one skip, which goes out over a drop of a group it never served and over nothing
\* else. A drop of a whole group names its span too, which may name again a start inside
\* it that was skipped or dropped before the group finished: the subscriber changes
\* nothing for it. No skip names a start that has a Group Stream, and no whole group's
\* span holds one.
NamedOnce ==
    /\ \A g \in Groups :
        /\ ndrop[g] <= 1
        /\ nskip[g] <= 1
        /\ nskip[g] > 0 => \A i \in 1..Len(streams) : streams[i].g # g
    /\ \A g \in Groups : ends[g] # 0 =>
        \A i \in 1..Len(streams) : streams[i].g \notin Inner(g, ends[g])

=============================================================================
