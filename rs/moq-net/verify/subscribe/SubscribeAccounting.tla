------------------------- MODULE SubscribeAccounting -------------------------
(***************************************************************************)
(* How one lite-07 subscription accounts for each group, between the       *)
(* publisher's `TrackRun` (rs/moq-net/src/lite/publisher.rs) and the       *)
(* subscriber's `TrackServe` (rs/moq-net/src/lite/subscriber.rs) with its  *)
(* `Tail` (rs/moq-net/src/tail.rs) and its copy of the track.              *)
(*                                                                         *)
(* The draft's promise: for every start in range the publisher sends a     *)
(* Group Stream, accounts for it with a GROUP_END, or names it in          *)
(* SUBSCRIBE_DROP (with SKIP for declared skips, or an error code). Every  *)
(* drop is final on its                                                    *)
(* subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens    *)
(* nothing, so updates are left out, and only a cut GROUP_END's skip goes  *)
(* out over a drop. Drops and skips travel in order on the Subscribe       *)
(* Stream; each Group Stream travels on its own, unordered against them.   *)
(* No timer settles anything: once quiet, the subscriber has accounted for *)
(* every start.                                                            *)
(*                                                                         *)
(* A Group Stream whose group expires stale resets with OLD, whether it    *)
(* never opened, is serving, or already sent its FIN (after a GROUP_END)   *)
(* and awaits the acknowledgement: stale bytes must not hold the send      *)
(* window. A subscriber that read it whole first keeps the group. A stream *)
(* reset while serving, evicted or preempted with TOO_FAR_BEHIND for a     *)
(* group that outranks it, is final. Under `ReliableReset` (RESET_STREAM_AT *)
(* negotiated), a reset once the transport took the GROUP header still     *)
(* delivers it, which accounts for the group, so nothing names it, though  *)
(* the reset may cut a GROUP_END after it, so the publisher skips the      *)
(* starts it told again. Any other reset is named in a drop: before then   *)
(* the header never left, and a plain RESET_STREAM lets the peer discard   *)
(* it unread, or deliver it.                                               *)
(*                                                                         *)
(* A whole group's span runs from its start to its End, and no group      *)
(* starts strictly inside it. Its GROUP_END accounts for the starts        *)
(* strictly inside the span, which the publisher then records as told, and *)
(* a drop of a group the publisher holds whole names the span. Every skip  *)
(* goes out as a SUBSCRIBE_DROP(SKIP). The track records a start inside a  *)
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
(* record runs as it doubles (`Fold::Final`): that one folds only skips,   *)
(* settled reports, and what a group held whole proves, a subset of what   *)
(* this may.                                                               *)
(*                                                                         *)
(* Scope: the subscription is already started (SUBSCRIBE_START) and covers *)
(* every start in `Groups`; ranges, fills, and frames are left out.       *)
(* An End is at most one past the highest start, so a span   *)
(* may run past the range. GROUP_END takeover is left out.                 *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,        \* starts in range
    Pressure,      \* the cache pool may hand the publisher's reader debt, so its record folds
    ReliableReset  \* the peer negotiated RESET_STREAM_AT (`Session::reliable_reset`)

\* The highest End a group may finish at: one past the highest start.
MaxEnd == 1 + CHOOSE m \in Groups : \A x \in Groups : x <= m

\* The starts strictly inside the span of a group starting at g and ending at e.
Inner(g, e) == {x \in Groups : g < x /\ x < e}

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err` ("old", "evicted", or a stop's "cancel")
\*   held: the transport took the GROUP header, so a reset may still deliver it
\*   fin: the FIN went out before any reset, so every byte may arrive first
\*   ended: the FIN followed a GROUP_END, accounting for the starts in `inner`, which
\*          a reset that cuts it skips again (`GroupOutcomes::cut`)
\*   sub: "none" (no header yet, or lost with a reset, or never sent), "header"
\*        (filling the copy), "fin", or "reset" (read to a reset, or stopped)
NewStream(g) ==
    [g |-> g, pub |-> "open", held |-> FALSE, fin |-> FALSE, ended |-> FALSE, inner |-> {},
     err |-> "", sub |-> "none"]

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
    outq,       \* `GroupOutcomes::dropped`: failed streams not yet named
    folded,     \* the record folded the group into its floor
    ndrop,      \* history: drops naming the group, not counting the span above it
    nskip,      \* history: skips naming the start, not counting one a cut GROUP_END sends
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    \* The subscriber's `Tail` and copy.
    acct,       \* the tail's ledger: starts accounted for
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached      \* the copy's group: "none", "open", "whole", "aborted", "evicted"

names == <<ndrop, nskip>>
pubVars == <<cur, late, ends, served, outq, folded, names>>
subVars == <<acct, rec, cached>>
vars == <<pubVars, streams, resp, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ ends = [g \in Groups |-> 0]
    /\ served = {} /\ outq = <<>> /\ folded = {}
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
    /\ UNCHANGED <<late, served, outq, folded, names, streams, resp, subVars>>

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
    /\ UNCHANGED <<ends, outq, folded, names, resp, subVars>>

\* `poll_forward_drop`: the cursor gives g up stale ("old"), final ("evicted"), or
\* declares it skipped, after `poll_step` named failed streams. A later verdict may follow
\* an earlier one. A group that finished whole exists, so it is never skipped.
\* `forward_range` sends it only where the record has told nothing: every report is final. A drop of a group held whole names its span.
CursorGiveUp(g, kind) ==
    /\ outq = <<>>
    /\ \/ cur[g] = "pending"
       \/ cur[g] = "stale" /\ kind \in {"evicted", "skip"}
       \/ cur[g] = "final" /\ kind = "skip"
    /\ kind = "skip" => ends[g] = 0
    /\ cur' = [cur EXCEPT ![g] = IF kind = "old" THEN "stale" ELSE IF kind = "skip" THEN "skip" ELSE "final"]
    /\ IF g \in served
       THEN UNCHANGED <<served, resp, names>>
       ELSE IF kind = "skip"
       THEN /\ served' = served \cup {g}
            /\ resp' = Append(resp, SkipMsg({g}))
            /\ nskip' = Named(nskip, {g})
            /\ UNCHANGED ndrop
       ELSE /\ served' = served \cup Span(g)
            /\ resp' = Append(resp, DropMsg(Span(g), kind))
            /\ ndrop' = Named(ndrop, {g})
            /\ UNCHANGED nskip
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
    /\ UNCHANGED <<cur, late, ends, outq, folded, ndrop, streams, subVars>>

\* A held stream's reset still delivers its header, which accounts for the group, only over
\* RESET_STREAM_AT: only then does `GroupState::Serve`'s first flush hold the stream there.
Kept(s) == s.held /\ ReliableReset

\* A reset that accounts for a stream's group may cut the GROUP_END after its header, so the
\* starts it told go out again as a skip (`GroupOutcomes::cut`), even one a drop named
\* before the group finished, which the skip changes nothing for.
CutSkip(s) ==
    IF s.inner = {}
    THEN UNCHANGED resp
    ELSE resp' = Append(resp, SkipMsg(s.inner))

\* The transport takes a serving stream's GROUP header, so the peer may read it however the
\* stream ends.
StreamHoldAt(i) ==
    /\ streams[i].pub = "open"
    /\ ~streams[i].held
    /\ streams' = [streams EXCEPT ![i].held = TRUE]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* A serving stream FINs after its last frame (`GroupState::Closed`). Once its group
\* finished whole, a GROUP_END goes first, and `buffer_group_end` records the starts
\* strictly inside the span as told. Otherwise the group is closed.
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams[i].held
    /\ LET g == streams[i].g
           ended == ends[g] # 0
           inner == IF ended THEN Inner(g, ends[g]) ELSE {}
       IN /\ streams' = [streams EXCEPT ![i].pub = "fin", ![i].fin = TRUE,
                                        ![i].ended = ended, ![i].inner = inner]
          /\ served' = served \cup inner
    /\ UNCHANGED <<cur, late, ends, outq, folded, names, resp, subVars>>

\* A stream fails and resets. It expires stale ("old", `poll_given_up`): before it opened,
\* waiting for stream credit (`give_way`, `shed`), so nothing of it reached the wire; while
\* serving; or after its FIN, before the acknowledgement (`GroupState::Closed`). Or it is
\* reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks
\* it, both final ("evicted"). A kept stream resets after its header (`Held::reset`) and
\* is accounted (`accounts`), its cut starts skipped again; `settle_failed` queues the
\* outcome of any other.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err]
    /\ IF Kept(streams[i])
       THEN CutSkip(streams[i]) /\ UNCHANGED outq
       ELSE outq' = Append(outq, <<streams[i].g, err>>) /\ UNCHANGED resp
    /\ UNCHANGED <<cur, late, ends, served, folded, names, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub = "fin"
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* `poll_step` names each failed stream whose reset may lose its header in a drop
\* with its error, final like any other: the group was served, so nothing serves or names
\* it again. A group held whole is named with its span, since no GROUP_END went out.
PubOutcomes ==
    /\ outq # <<>>
    /\ outq' = Tail(outq)
    /\ LET g == Head(outq)[1]
           gs == Span(g)
       IN /\ resp' = Append(resp, DropMsg(gs, Head(outq)[2]))
          /\ served' = served \cup gs
          /\ ndrop' = Named(ndrop, {g})
    /\ UNCHANGED <<cur, late, ends, folded, nskip, streams, subVars>>

\* The pool hands the reader debt and its record folds the start into its floor: one the
\* cursor reached a verdict on, once no failed stream of it is still to name. Every
\* report is final, so the fold forgets which.
PubFold(g) ==
    /\ Pressure
    /\ g \notin folded
    /\ cur[g] # "pending"
    /\ \A i \in 1..Len(outq) : outq[i][1] # g
    /\ folded' = folded \cup {g}
    /\ UNCHANGED <<cur, late, ends, served, outq, names, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group, and the stream fills the copy, replacing a stale record of it.
\* A reset loses a header the transport had not taken, and a plain reset may lose one it had.
SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].held \/ streams[i].pub = "open"
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
\* error. The header already accounted for the group. A reset after the header cuts what
\* follows it, a GROUP_END included.
SubResetAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].pub = "reset"
    /\ streams' = [streams EXCEPT ![i].sub = "reset"]
    /\ cached' = [cached EXCEPT ![streams[i].g] = "aborted"]
    /\ UNCHANGED <<pubVars, resp, acct, rec>>

\* The subscriber stops a stream whose header it read (`UniServe::abort` on a failed
\* `GroupRecv`), with EVICTED or any other code (Cancel, Duplicate), and its copy drops the group.
\* The publisher answers with a reset (`accounts`): the stop owes a drop only when it is
\* EVICTED and arrived before the header's acknowledgement, which `acked` chooses freely, so
\* the subscriber may have stopped the stream unsure which group it was. Any other stop the
\* subscriber sent knowing the group, and the publisher skips the starts its GROUP_END told
\* again, since the stop cuts a GROUP_END the subscriber had not read.
SubStopAt(i, err, acked) ==
    /\ streams[i].sub = "header"
    /\ streams[i].pub \in {"open", "fin"}
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err, ![i].sub = "reset"]
    /\ cached' = [cached EXCEPT ![streams[i].g] = "aborted"]
    /\ IF err = "evicted" /\ ~(Kept(streams[i]) /\ acked)
       THEN outq' = Append(outq, <<streams[i].g, err>>) /\ UNCHANGED resp
       ELSE CutSkip(streams[i]) /\ UNCHANGED outq
    /\ UNCHANGED <<cur, late, ends, served, folded, names, acct, rec>>

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
\* start, as of a whole group's span, changes nothing.
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

\* `apply_response` for SUBSCRIBE_SKIP: final, but a group that arrived keeps it
\* (`skip_received`).
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
\* read to the reset or to the FIN before it, or, never held, lost or never sent.
StreamDone(s) ==
    \/ s.pub = "acked"
    \/ s.pub = "reset" /\ (s.sub \in {"reset", "fin"} \/ (s.sub = "none" /\ ~Kept(s)))

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
StreamHold == \E i \in 1..Len(streams) : StreamHoldAt(i)
StreamFin == \E i \in 1..Len(streams) : StreamFinAt(i)
StreamFail == \E i \in 1..Len(streams), err \in {"old", "evicted"} : StreamFailAt(i, err)
StreamAck == \E i \in 1..Len(streams) : StreamAckAt(i)
SubHeader == \E i \in 1..Len(streams) : SubHeaderAt(i)
SubFin == \E i \in 1..Len(streams) : SubFinAt(i)
SubReset == \E i \in 1..Len(streams) : SubResetAt(i)
SubStop == \E i \in 1..Len(streams), err \in {"evicted", "cancel"}, acked \in BOOLEAN : SubStopAt(i, err, acked)

Next ==
    \/ \E g \in Groups, e \in 1..MaxEnd : g < e /\ Finish(g, e)
    \/ \E g \in Groups : CursorHand(g)
    \/ \E g \in Groups, kind \in {"old", "evicted", "skip"} : CursorGiveUp(g, kind)
    \/ \E g \in Groups : CursorHole(g)
    \/ StreamHold
    \/ StreamFin
    \/ StreamFail
    \/ StreamAck
    \/ PubOutcomes
    \/ \E g \in Groups : PubFold(g)
    \/ SubHeader
    \/ SubFin
    \/ SubReset
    \/ SubStop
    \/ \E g \in Groups : Evict(g)
    \/ SubRecv
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules                                                                   *)
-----------------------------------------------------------------------------

\* Once quiet, the tail accounts for every start, those a reset cut out of a GROUP_END
\* included: none is left to a timer.
QuietIsSettled == Quiet => acct = Groups

\* The publisher opens at most one Group Stream for a group, so the subscriber never reads
\* two at once and no group's bytes go out twice.
ServedOnce ==
    \A i, j \in 1..Len(streams) : i # j => streams[i].g # streams[j].g

\* Every name is final: the publisher names a group in at most one drop and a start in at
\* most one skip, and skips no start it served or dropped, besides the starts a reset cut
\* out of a GROUP_END. A drop of a whole group names its span too, which may name again a
\* start inside it that was skipped before the group finished. The subscriber changes
\* nothing for a start named again.
NamedOnce == \A g \in Groups : ndrop[g] <= 1 /\ nskip[g] <= 1

\* No skip names a start that has a Group Stream, and no whole group's span holds one.
NoContradictorySkip ==
    /\ \A g \in Groups : nskip[g] > 0 => \A i \in 1..Len(streams) : streams[i].g # g
    /\ \A g \in Groups : ends[g] # 0 =>
        \A i \in 1..Len(streams) : streams[i].g \notin Inner(g, ends[g])

=============================================================================
