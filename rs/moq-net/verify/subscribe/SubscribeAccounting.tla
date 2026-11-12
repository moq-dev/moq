------------------------- MODULE SubscribeAccounting -------------------------
(***************************************************************************)
(* How one lite-07 subscription accounts for each group, between the       *)
(* publisher's `TrackRun` (rs/moq-net/src/lite/publisher.rs) and the       *)
(* subscriber's `TrackServe` (rs/moq-net/src/lite/subscriber.rs) with its  *)
(* `Tail` (rs/moq-net/src/tail.rs) and its copy of the track.              *)
(*                                                                         *)
(* The draft's promise: for every sequence in range the publisher sends a  *)
(* Group Stream or names it in SUBSCRIBE_SKIP or SUBSCRIBE_DROP. A drop of *)
(* OLD is a stale verdict under the update it names, which the next        *)
(* SUBSCRIBE_UPDATE reopens; a Group Stream wins over a stale drop, even   *)
(* one it overtook. Drops and skips travel in order on the Subscribe       *)
(* Stream; each Group Stream travels on its own, unordered against them,   *)
(* and a reset can lose its header.                                        *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event. The track     *)
(* cursor's verdicts (hand a group out, give it up stale or final, skip    *)
(* it) and whether the cache still holds a fresh copy on an update are     *)
(* free choices, standing in for the cache, the clock, and the routes.     *)
(* Under `Pressure`, so is when the pool's debt folds what each side keeps *)
(* of the group: the publisher's record and the subscriber's tail.         *)
(*                                                                         *)
(* Scope: the subscription is already answered (SUBSCRIBE_OK) and covers   *)
(* every group; ranges, starts, fills, and the tail's grace are left out.  *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,      \* group sequences in range
    MaxUpdates,  \* SUBSCRIBE_UPDATEs the subscriber sends
    MaxStreams,  \* Group Streams the publisher opens
    Pressure     \* the cache pool may hand the readers debt, so each side folds

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err`
\*   sub: "none" (no header yet, or lost with a reset), "header", "fin", "reset"
NewStream(g) == [g |-> g, pub |-> "open", err |-> "", wasFin |-> FALSE, sub |-> "none"]

DropMsg(g, err, u) == [type |-> "drop", g |-> g, err |-> err, u |-> u]
SkipMsg(g) == [type |-> "skip", g |-> g, err |-> "", u |-> 0]

VARIABLES
    \* The publisher's track cursor: its verdict on each group.
    cur,        \* "pending", "handed", "stale", "final", or "skip"
    late,       \* the cursor handed the group out once more
    \* The publisher's `TrackRun`.
    pup,        \* `update`: the number in effect
    served,     \* `served`: sequences served or given up
    dropped,    \* `dropped`: named in a drop, never served
    dstale,     \* `dropped_stale`: the part of `dropped` named stale
    expired,    \* `expired`: groups whose stream expired as stale
    redrop,     \* `redrop`: expired runs an update owes a drop again
    reoffered,  \* `reoffered`: expired groups an update serves again
    outq,       \* `GroupOutcomes::dropped`: failed streams not yet named
    folded,     \* the record folded the group into its floor (`Record::folded`)
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    ctrl,       \* the Subscribe Stream, subscriber to publisher: update numbers
    \* The subscriber's `SubStream`, `Tail`, and copy.
    sup,        \* `SubStream::update`: the latest number sent
    acct,       \* the tail's ledger: groups accounted for
    finished,   \* `Tail::finished`: groups whose stream finished since the last update
    sstale,     \* `SubStream::stale`: runs accounted by a stale drop under `sup`
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached,     \* the copy's group: "none", "open", "whole", "aborted", "evicted"
    got,        \* history: the subscriber read the group to its FIN
    violation   \* the subscriber closed the session with a PROTOCOL_VIOLATION

pubVars == <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered, outq, folded>>
subVars == <<sup, acct, finished, sstale, rec, cached, got, violation>>
vars == <<pubVars, streams, resp, ctrl, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ pup = 0
    /\ served = {} /\ dropped = {} /\ dstale = {} /\ expired = {}
    /\ redrop = {} /\ reoffered = {} /\ outq = <<>> /\ folded = {}
    /\ streams = <<>> /\ resp = <<>> /\ ctrl = <<>>
    /\ sup = 0
    /\ acct = {} /\ finished = {} /\ sstale = {}
    /\ rec = [g \in Groups |-> "none"]
    /\ cached = [g \in Groups |-> "none"]
    /\ got = {}
    /\ violation = FALSE

-----------------------------------------------------------------------------
(* Publisher                                                               *)
-----------------------------------------------------------------------------

\* `replace_stale`: a verdict or group replaces a drop of g.
ReplaceStale(g) ==
    /\ dropped' = dropped \ {g}
    /\ dstale' = dstale \ {g}
    /\ expired' = expired \ {g}
    /\ redrop' = redrop \ {g}

\* `serve`: open a Group Stream for g, which `mark_served` records.
Serve(g) ==
    /\ Len(streams) < MaxStreams
    /\ streams' = Append(streams, NewStream(g))
    /\ served' = served \cup {g}

\* `poll_track` takes `Recv::Group`: the cursor hands g out. Once more, it may hand out a
\* group it gave up: one a route the front switched to delivers after a stale verdict, or
\* one re-created after its drop. It never hands out a sequence twice. A group handed out
\* after its stale drop is served over it; any other sequence already accounted for is not.
\* `poll_step` names failed streams and drops again first, and a re-offered group takes
\* the next stream ahead of the cursor's.
CursorHand(g) ==
    /\ outq = <<>> /\ redrop = {} /\ reoffered = {}
    /\ g \notin folded
    /\ \/ cur[g] = "pending" /\ UNCHANGED late
       \/ cur[g] \in {"stale", "final"} /\ ~late[g] /\ late' = [late EXCEPT ![g] = TRUE]
    /\ cur' = [cur EXCEPT ![g] = "handed"]
    /\ IF g \in dstale
       THEN /\ ReplaceStale(g)
            /\ Serve(g)
       ELSE IF g \in served
            THEN UNCHANGED <<served, dropped, dstale, expired, redrop, streams>>
            ELSE /\ Serve(g)
                 /\ UNCHANGED <<dropped, dstale, expired, redrop>>
    /\ UNCHANGED <<pup, reoffered, outq, folded, resp, ctrl, subVars>>

\* `poll_forward_drop`: the cursor gives g up stale ("old"), final ("evicted"), or
\* declares it skipped, after `poll_step` named failed streams and dropped again. A later
\* verdict may replace an earlier one. `forward_range` replaces what the verdict
\* overrides, then `drop_range` names g only if it is not yet served or given up.
CursorGiveUp(g, kind) ==
    /\ outq = <<>> /\ redrop = {}
    /\ \/ cur[g] = "pending"
       \/ cur[g] = "stale" /\ kind \in {"evicted", "skip"}
       \/ cur[g] = "final" /\ kind = "skip"
    /\ cur' = [cur EXCEPT ![g] = IF kind = "old" THEN "stale" ELSE IF kind = "skip" THEN "skip" ELSE "final"]
    /\ LET replaces == CASE kind = "skip" -> dropped
                         [] kind = "old" -> {}
                         [] OTHER -> dstale
           hit == g \in replaces
           s1 == IF hit THEN served \ {g} ELSE served
           d1 == IF hit THEN dropped \ {g} ELSE dropped
           ds1 == IF hit THEN dstale \ {g} ELSE dstale
           named == g \notin s1
       IN /\ served' = s1 \cup {g}
          /\ dropped' = IF ~named THEN d1
                        ELSE IF kind = "skip" THEN d1 \ {g} ELSE d1 \cup {g}
          /\ dstale' = IF ~named THEN ds1
                       ELSE IF kind = "old" THEN ds1 \cup {g} ELSE ds1 \ {g}
          /\ expired' = IF hit THEN expired \ {g} ELSE expired
          /\ redrop' = IF hit THEN redrop \ {g} ELSE redrop
          /\ resp' = IF ~named THEN resp
                     ELSE IF kind = "skip" THEN Append(resp, SkipMsg(g))
                     ELSE Append(resp, DropMsg(g, kind, pup))
    /\ UNCHANGED <<late, pup, reoffered, outq, folded, streams, ctrl, subVars>>

\* A serving stream writes its last frame and FINs (`GroupState::Closed`).
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams' = [streams EXCEPT ![i].pub = "fin"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* A stream fails and resets: it expires stale while serving or while its FIN awaits
\* the acknowledgement, or is evicted while serving. `settle_failed` queues the outcome.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err,
                                  ![i].wasFin = (streams[i].pub = "fin")]
    /\ outq' = Append(outq, <<streams[i].g, err>>)
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered,
                   folded, resp, ctrl, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub \in {"fin", "dup"}
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* `poll_step` names the failed streams: a stale expiry is a stale drop the next
\* update judges again, anything else a final one. A stale expiry of a group the
\* record folded while its stream was open is final too: no update re-offers it.
PubOutcomes ==
    /\ outq # <<>>
    /\ LET g == Head(outq)[1]
           err == IF Head(outq)[2] = "old" /\ g \in folded THEN "evicted" ELSE Head(outq)[2]
       IN /\ outq' = Tail(outq)
          /\ resp' = Append(resp, DropMsg(g, err, pup))
          /\ IF err = "old"
             THEN /\ expired' = expired \cup {g}
                  /\ dropped' = dropped \cup {g}
                  /\ dstale' = dstale \cup {g}
             ELSE UNCHANGED <<expired, dropped, dstale>>
    /\ UNCHANGED <<cur, late, pup, served, redrop, reoffered, folded, streams, ctrl, subVars>>

\* `poll_step` drops again, under the new number, what an update could not re-offer.
PubRedrop ==
    /\ outq = <<>>
    /\ redrop # {}
    /\ LET g == CHOOSE x \in redrop : \A y \in redrop : x <= y
       IN /\ resp' = Append(resp, DropMsg(g, "old", pup))
          /\ redrop' = redrop \ {g}
          /\ expired' = expired \cup {g}
          /\ dropped' = dropped \cup {g}
          /\ dstale' = dstale \cup {g}
          /\ served' = served \cup {g}
    /\ UNCHANGED <<cur, late, pup, reoffered, outq, folded, streams, ctrl, subVars>>

\* `poll_track` serves a re-offered group, unless a verdict since accounted for it. One
\* the record folded while it waited is named in a final drop instead.
PubReoffer ==
    /\ outq = <<>> /\ redrop = {}
    /\ reoffered # {}
    /\ LET g == CHOOSE x \in reoffered : \A y \in reoffered : x <= y
       IN /\ reoffered' = reoffered \ {g}
          /\ CASE g \in folded ->
                    /\ resp' = Append(resp, DropMsg(g, "evicted", pup))
                    /\ UNCHANGED <<served, streams>>
               [] g \in served -> UNCHANGED <<served, streams, resp>>
               [] OTHER -> Serve(g) /\ UNCHANGED resp
    /\ UNCHANGED <<cur, late, pup, dropped, dstale, expired, redrop, outq, folded, ctrl, subVars>>

\* The pool hands the reader debt and its record folds the group into its floor: a group
\* handed out or named final, once nothing about it is still to tell. The fold stops at a
\* stale report the next update may reopen and at a failed stream not yet named.
PubFold(g) ==
    /\ Pressure
    /\ g \notin folded
    /\ cur[g] \in {"handed", "final", "skip"}
    /\ g \notin dstale
    /\ \A i \in 1..Len(outq) : outq[i][1] # g
    /\ folded' = folded \cup {g}
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered, outq,
                   streams, resp, ctrl, subVars>>

\* `TrackRun::update` applies a SUBSCRIBE_UPDATE: the cursor's stale verdicts reopen
\* (`reoffer_stale`, then `owe`), and each expired stream is judged again: served
\* again where the cache still holds a fresh copy (`reoffer_groups`), else dropped again.
PubUpdate ==
    /\ ctrl # <<>>
    /\ LET u == Head(ctrl)
           reopened == {g \in Groups : cur[g] = "stale"}
           judged == expired \cup reoffered \cup redrop
       IN /\ ctrl' = Tail(ctrl)
          /\ pup' = u
          /\ cur' = [g \in Groups |-> IF g \in reopened THEN "pending" ELSE cur[g]]
          /\ \E fresh \in SUBSET judged :
                LET owed == reopened \cup fresh
                IN /\ reoffered' = fresh
                   /\ redrop' = judged \ fresh
                   /\ expired' = {}
                   /\ served' = served \ owed
                   /\ dropped' = dropped \ owed
                   /\ dstale' = dstale \ owed
    /\ UNCHANGED <<late, outq, folded, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* The streams of g waiting, unread, for the copy another stream is filling.
Held(g) == {i \in 1..Len(streams) : streams[i].g = g /\ streams[i].sub = "held"}

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group. It fills the copy and replaces a stale record of it, unless
\* another stream is filling the copy (`Open::Duplicate`, held until that one settles)
\* or the copy holds the group whole (`Error::Duplicate`).
SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].pub \in {"open", "fin", "reset"}
    /\ LET g == streams[i].g
       IN /\ acct' = acct \cup {g}
          /\ CASE cached[g] = "open" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "held"]
                    /\ UNCHANGED <<cached, rec>>
               [] cached[g] = "whole" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "dup"]
                    /\ UNCHANGED <<cached, rec>>
               [] OTHER ->
                    /\ streams' = [streams EXCEPT ![i].sub = "header"]
                    /\ cached' = [cached EXCEPT ![g] = "open"]
                    /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, finished, sstale, got, violation>>

\* The stream filling the copy is read to its FIN (`Reading::finish`); once every byte
\* arrived, a reset sent after the FIN changes nothing. A held stream of the group then
\* only duplicates it.
SubFinAt(i) ==
    /\ streams[i].sub = "header"
    /\ \/ streams[i].pub \in {"fin", "acked"}
       \/ streams[i].pub = "reset" /\ streams[i].wasFin
    /\ LET g == streams[i].g
       IN /\ streams' = [j \in 1..Len(streams) |->
                            IF j = i THEN [streams[j] EXCEPT !.sub = "fin"]
                            ELSE IF j \in Held(g) THEN [streams[j] EXCEPT !.sub = "dup"]
                            ELSE streams[j]]
          /\ finished' = finished \cup {g}
          /\ cached' = [cached EXCEPT ![g] = "whole"]
          /\ got' = got \cup {g}
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, acct, sstale, rec, violation>>

\* The reset of the stream filling the copy arrives: the group's readers end with its
\* error, and the first held stream of the group opens it again (`TrackEntry::reopen`).
SubResetAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].pub = "reset"
    /\ LET g == streams[i].g
           next == {j \in Held(g) : \A k \in Held(g) : j <= k}
       IN /\ streams' = [j \in 1..Len(streams) |->
                            IF j = i THEN [streams[j] EXCEPT !.sub = "reset"]
                            ELSE IF j \in next THEN [streams[j] EXCEPT !.sub = "header"]
                            ELSE streams[j]]
          /\ cached' = [cached EXCEPT ![g] = IF next = {} THEN "aborted" ELSE "open"]
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, acct, finished, sstale, rec, got, violation>>

\* A held stream's own reset arrives.
SubResetHeldAt(i) ==
    /\ streams[i].sub = "held"
    /\ streams[i].pub = "reset"
    /\ streams' = [streams EXCEPT ![i].sub = "reset"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* The copy's cache evicts a whole group.
Evict(g) ==
    /\ cached[g] = "whole"
    /\ cached' = [cached EXCEPT ![g] = "evicted"]
    /\ UNCHANGED <<pubVars, streams, resp, ctrl, sup, acct, finished, sstale, rec, got, violation>>

\* The pool hands the tail debt and `Tail::rebill` forgets every finished run but the
\* newest: one group stands for any of them, since a later group may have finished.
TailFold(g) ==
    /\ Pressure
    /\ g \in finished
    /\ finished' = finished \ {g}
    /\ UNCHANGED <<pubVars, streams, resp, ctrl, sup, acct, sstale, rec, cached, got, violation>>

\* `apply_response` for SUBSCRIBE_DROP. A stale drop under a replaced update is no
\* verdict. Otherwise the tail accounts for the group; a stale drop records it in the
\* copy unless its stream finished under this update or the copy holds it
\* (`give_up` keeps an open group), and a final drop records it and ends its readers.
OnDrop(m) ==
    LET g == m.g
        stale == m.err = "old"
    IN IF m.u > sup
       THEN /\ violation' = TRUE
            /\ UNCHANGED <<acct, sstale, rec, cached>>
       ELSE IF stale /\ m.u < sup
       THEN UNCHANGED <<acct, sstale, rec, cached, violation>>
       ELSE IF stale
       THEN LET recorded == g \notin finished
                            /\ cached[g] \notin {"open", "whole"}
                            /\ rec[g] # "skip"
            IN /\ acct' = acct \cup {g}
               /\ rec' = IF recorded THEN [rec EXCEPT ![g] = "stale"] ELSE rec
               /\ sstale' = IF recorded THEN sstale \cup {g} ELSE sstale
               /\ UNCHANGED <<cached, violation>>
       ELSE /\ acct' = acct \cup {g}
            /\ sstale' = sstale \ {g}
            /\ rec' = [rec EXCEPT ![g] = IF @ = "skip" THEN "skip" ELSE "final"]
            /\ cached' = [cached EXCEPT ![g] = IF @ = "open" THEN "aborted" ELSE @]
            /\ UNCHANGED violation

\* `apply_response` for SUBSCRIBE_SKIP: final, overriding any drop, but a group that
\* arrived keeps it (`skip_received`).
OnSkip(m) ==
    LET g == m.g
    IN /\ acct' = acct \cup {g}
       /\ sstale' = sstale \ {g}
       /\ rec' = IF cached[g] \in {"open", "whole"} THEN rec ELSE [rec EXCEPT ![g] = "skip"]
       /\ UNCHANGED <<cached, violation>>

SubRecv ==
    /\ resp # <<>>
    /\ resp' = Tail(resp)
    /\ LET m == Head(resp)
       IN IF m.type = "drop" THEN OnDrop(m) ELSE OnSkip(m)
    /\ UNCHANGED <<pubVars, streams, ctrl, sup, finished, got>>

\* `buffer_update`: the copy forgets every stale record (`supersede_stale`), the tail
\* owes again each stale run this subscription accounted that the copy still held,
\* and `Tail::updated` forgets which streams finished.
SubUpdate ==
    /\ sup < MaxUpdates
    /\ ~violation
    /\ LET held == {g \in Groups : rec[g] = "stale"}
       IN /\ sup' = sup + 1
          /\ ctrl' = Append(ctrl, sup + 1)
          /\ rec' = [g \in Groups |-> IF rec[g] = "stale" THEN "none" ELSE rec[g]]
          /\ acct' = acct \ (sstale \cap held)
          /\ sstale' = {}
          /\ finished' = {}
    /\ UNCHANGED <<pubVars, streams, resp, cached, got, violation>>

-----------------------------------------------------------------------------

\* A stream is done once both sides stopped: acknowledged, or reset with the header
\* read to the reset or lost.
StreamDone(s) ==
    \/ s.pub = "acked"
    \/ s.pub = "reset" /\ s.sub \in {"none", "fin", "reset", "dup"}

\* Nothing in flight and nothing left for the publisher to do.
Quiet ==
    /\ resp = <<>> /\ ctrl = <<>> /\ outq = <<>>
    /\ redrop = {} /\ reoffered = {}
    /\ \A g \in Groups : cur[g] # "pending"
    /\ \A i \in 1..Len(streams) : StreamDone(streams[i])

\* The end: quiet, or out of Group Streams to open.
Done ==
    /\ Quiet \/ Len(streams) = MaxStreams
    /\ UNCHANGED vars

\* Each action over the streams, named so a trace says which one ran.
StreamFin == \E i \in 1..Len(streams) : StreamFinAt(i)
StreamFail == \E i \in 1..Len(streams), err \in {"old", "evicted"} : StreamFailAt(i, err)
StreamAck == \E i \in 1..Len(streams) : StreamAckAt(i)
SubHeader == \E i \in 1..Len(streams) : SubHeaderAt(i)
SubFin == \E i \in 1..Len(streams) : SubFinAt(i)
SubReset == \E i \in 1..Len(streams) : SubResetAt(i)
SubResetHeld == \E i \in 1..Len(streams) : SubResetHeldAt(i)

Next ==
    \/ \E g \in Groups : CursorHand(g)
    \/ \E g \in Groups, kind \in {"old", "evicted", "skip"} : CursorGiveUp(g, kind)
    \/ StreamFin
    \/ StreamFail
    \/ StreamAck
    \/ PubOutcomes
    \/ PubRedrop
    \/ PubReoffer
    \/ PubUpdate
    \/ \E g \in Groups : PubFold(g)
    \/ SubHeader
    \/ SubFin
    \/ SubReset
    \/ SubResetHeld
    \/ \E g \in Groups : Evict(g)
    \/ \E g \in Groups : TailFold(g)
    \/ SubRecv
    \/ SubUpdate
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules                                                                   *)
-----------------------------------------------------------------------------

\* Once quiet, the tail accounts for every group: none is waited for until the grace.
QuietIsSettled == Quiet => acct = Groups

\* A group read to its FIN is never recorded stale: the stream wins over the drop.
StreamWins == \A g \in got : rec[g] # "stale"

\* The publisher serves a group again only to re-offer one whose stream it reset.
ServedOnce ==
    \A i, j \in 1..Len(streams) :
        (i < j /\ streams[i].g = streams[j].g) => streams[i].pub = "reset"

\* A drop never names an update the subscriber has not sent.
NoViolation == ~violation

=============================================================================
