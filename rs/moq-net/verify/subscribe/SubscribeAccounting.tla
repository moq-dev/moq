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
(* A publisher out of stream credit may reset a Group Stream with YIELDED, *)
(* which pauses the group instead of accounting for it: the subscriber     *)
(* keeps the frames it read, and the publisher later continues the group   *)
(* on a new Group Stream from the reset stream's own Frame Start, or names *)
(* it in a drop. A continuation may overtake the YIELDED reset and waits,  *)
(* unread, for it. It is the only second Group Stream of a group.          *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event. The track     *)
(* cursor's verdicts (hand a group out, give it up stale or final, skip    *)
(* it) are free choices, standing in for the cache, the clock, and the     *)
(* routes. So are when the publisher yields a stream, and whether it       *)
(* continues a paused group or gives it up, standing in for stream credit  *)
(* and expiry. Under `Pressure`, so is when the pool's debt folds the      *)
(* publisher's record of the group.                                        *)
(*                                                                         *)
(* Scope: the subscription is already answered (SUBSCRIBE_OK) and covers   *)
(* every group; ranges, starts, fills, and the tail's grace are left out.  *)
(* GROUP_END takeover is left out: only the YIELDED reset pauses a group.  *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,      \* group sequences in range
    MaxStreams,  \* Group Streams the publisher opens
    MaxPauses,   \* YIELDED resets the publisher sends per group
    Frames,      \* frames in each group: 0 leaves frames out
    Pressure     \* the cache pool may hand the publisher's reader debt, so its record folds

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err` ("old", "evicted", or "yielded")
\*   sub: "none" (no header yet, or lost with a reset), "header" (filling the copy),
\*        "held" (waiting on the stream filling the copy), "fin", "reset", or "dup"
\*   of: the stream this one continues, or 0 for a fresh one
\*   start: its Frame Start; wrote: frames the publisher wrote on it; read: frames the
\*        subscriber read from it
NewStream(g, of, start) ==
    [g |-> g, pub |-> "open", err |-> "", wasFin |-> FALSE, sub |-> "none",
     of |-> of, start |-> start, wrote |-> 0, read |-> 0]

DropMsg(g, err) == [type |-> "drop", g |-> g, err |-> err]
SkipMsg(g) == [type |-> "skip", g |-> g, err |-> ""]

Max(a, b) == IF a > b THEN a ELSE b

VARIABLES
    \* The publisher's track cursor: its verdict on each group.
    cur,        \* "pending", "handed", "stale", "final", or "skip"
    late,       \* the cursor handed the group out once more
    \* The publisher's `TrackRun` and its reader's record.
    served,     \* `Record::told`: sequences served or given up
    dropped,    \* the part of `served` named in a drop, never served (`Report::Dropped`)
    outq,       \* `GroupOutcomes::dropped`: failed streams not yet named
    folded,     \* the record folded the group into its floor
    paused,     \* the YIELDED stream a paused group waits to continue, or 0
    pauses,     \* YIELDED resets sent for the group
    ndrop,      \* history: drops naming the group
    nskip,      \* history: skips naming the group
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    \* The subscriber's `Tail` and copy.
    acct,       \* the tail's ledger: groups accounted for
    reading,    \* the part of `acct` only the stream filling the copy accounts for
    sdrop,      \* groups a drop named (`TrackEntry::dropped`)
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached,     \* the copy's group: "none", "open", "paused", "whole", "aborted", "evicted"
    have,       \* the index after the last frame the copy holds of the group
    got,        \* history: the subscriber read the group to its FIN
    malformed   \* a continuation started past the frames the copy holds

names == <<ndrop, nskip>>
pubVars == <<cur, late, served, dropped, outq, folded, paused, pauses, names>>
subVars == <<acct, reading, sdrop, rec, cached, have, got, malformed>>
vars == <<pubVars, streams, resp, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ served = {} /\ dropped = {} /\ outq = <<>> /\ folded = {}
    /\ paused = [g \in Groups |-> 0]
    /\ pauses = [g \in Groups |-> 0]
    /\ ndrop = [g \in Groups |-> 0]
    /\ nskip = [g \in Groups |-> 0]
    /\ streams = <<>> /\ resp = <<>>
    /\ acct = {} /\ reading = {} /\ sdrop = {}
    /\ rec = [g \in Groups |-> "none"]
    /\ cached = [g \in Groups |-> "none"]
    /\ have = [g \in Groups |-> 0]
    /\ got = {}
    /\ malformed = FALSE

-----------------------------------------------------------------------------
(* Publisher                                                               *)
-----------------------------------------------------------------------------

\* `serve`: open a Group Stream for g, which `mark_served` records.
Serve(g) ==
    /\ Len(streams) < MaxStreams
    /\ streams' = Append(streams, NewStream(g, 0, 0))
    /\ served' = served \cup {g}

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
       THEN UNCHANGED <<served, streams>>
       ELSE Serve(g)
    /\ UNCHANGED <<dropped, outq, folded, paused, pauses, names, resp, subVars>>

\* `poll_forward_drop`: the cursor gives g up stale ("old"), final ("evicted"), or
\* declares it skipped, after `poll_step` named failed streams. A later verdict may follow
\* an earlier one. `forward_range` sends it only where the record has told nothing, or, for
\* a skip, only a drop (`Report::blocks`).
CursorGiveUp(g, kind) ==
    /\ outq = <<>>
    /\ \/ cur[g] = "pending"
       \/ cur[g] = "stale" /\ kind \in {"evicted", "skip"}
       \/ cur[g] = "final" /\ kind = "skip"
    /\ cur' = [cur EXCEPT ![g] = IF kind = "old" THEN "stale" ELSE IF kind = "skip" THEN "skip" ELSE "final"]
    /\ LET named == g \notin served \/ (kind = "skip" /\ g \in dropped)
       IN /\ served' = served \cup {g}
          /\ dropped' = IF ~named THEN dropped
                        ELSE IF kind = "skip" THEN dropped \ {g} ELSE dropped \cup {g}
          /\ resp' = IF ~named THEN resp
                     ELSE IF kind = "skip" THEN Append(resp, SkipMsg(g))
                     ELSE Append(resp, DropMsg(g, kind))
          /\ ndrop' = IF named /\ kind # "skip" THEN [ndrop EXCEPT ![g] = @ + 1] ELSE ndrop
          /\ nskip' = IF named /\ kind = "skip" THEN [nskip EXCEPT ![g] = @ + 1] ELSE nskip
    /\ UNCHANGED <<late, outq, folded, paused, pauses, streams, subVars>>

\* A serving stream writes its next frame.
StreamWriteAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams[i].start + streams[i].wrote < Frames
    /\ streams' = [streams EXCEPT ![i].wrote = @ + 1]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* A serving stream writes its last frame and FINs (`GroupState::Closed`).
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams[i].start + streams[i].wrote = Frames
    /\ streams' = [streams EXCEPT ![i].pub = "fin"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* A stream fails and resets: it expires stale while serving or while its FIN awaits
\* the acknowledgement, or is evicted while serving. `settle_failed` queues the outcome.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err,
                                  ![i].wasFin = (streams[i].pub = "fin")]
    /\ outq' = Append(outq, <<streams[i].g, err>>)
    /\ UNCHANGED <<cur, late, served, dropped, folded, paused, pauses, names, resp, subVars>>

\* Out of stream credit, the publisher resets a serving stream with YIELDED. The group is
\* paused, not given up: nothing names it, and the publisher owes a continuation or a drop.
StreamYieldAt(i) ==
    /\ streams[i].pub = "open"
    /\ pauses[streams[i].g] < MaxPauses
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = "yielded"]
    /\ paused' = [paused EXCEPT ![streams[i].g] = i]
    /\ pauses' = [pauses EXCEPT ![streams[i].g] = @ + 1]
    /\ UNCHANGED <<cur, late, served, dropped, outq, folded, names, resp, subVars>>

\* The publisher continues a paused group on a new Group Stream, from the YIELDED
\* stream's own Frame Start: a reset can discard frames the transport acknowledged.
PubContinue(g) ==
    /\ paused[g] # 0
    /\ Len(streams) < MaxStreams
    /\ LET k == paused[g]
       IN streams' = Append(streams, NewStream(g, k, streams[k].start))
    /\ paused' = [paused EXCEPT ![g] = 0]
    /\ UNCHANGED <<cur, late, served, dropped, outq, folded, pauses, names, resp, subVars>>

\* The publisher gives a paused group up instead, as it would the stream it yielded:
\* it expires stale under the delivery budget, or is evicted.
PausedGiveUp(g, err) ==
    /\ paused[g] # 0
    /\ paused' = [paused EXCEPT ![g] = 0]
    /\ outq' = Append(outq, <<g, err>>)
    /\ UNCHANGED <<cur, late, served, dropped, folded, pauses, names, streams, resp, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub \in {"fin", "dup"}
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* `poll_step` names each failed stream in a drop with its error, final like any other:
\* the group was served, so nothing serves or names it again.
PubOutcomes ==
    /\ outq # <<>>
    /\ outq' = Tail(outq)
    /\ resp' = Append(resp, DropMsg(Head(outq)[1], Head(outq)[2]))
    /\ ndrop' = [ndrop EXCEPT ![Head(outq)[1]] = @ + 1]
    /\ UNCHANGED <<cur, late, served, dropped, folded, paused, pauses, nskip, streams, subVars>>

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
    /\ UNCHANGED <<cur, late, served, outq, paused, pauses, names, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* The streams of g waiting, unread, for the copy another stream is filling.
Held(g) == {i \in 1..Len(streams) : streams[i].g = g /\ streams[i].sub = "held"}

\* A stream of g is still being read into the copy, even one a final drop aborted.
Filling(g) == \E j \in 1..Len(streams) : streams[j].g = g /\ streams[j].sub = "header"

\* The ledger already accounts for g, not only through a stream still reading it: a
\* stream that finished it, a reset, a drop, or a skip. A stream that starts filling the
\* copy then adds nothing a YIELDED reset of it could take back.
Settled(g) == g \in acct /\ g \notin reading

\* The first of them, which takes the copy over once the filling stream ends.
NextHeld(g) == {j \in Held(g) : \A k \in Held(g) : j <= k}

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group. It fills the copy and replaces a stale record of it, unless
\* another stream of it is still read (`Open::Duplicate`, held until that one settles)
\* or the copy holds the group whole (`Error::Duplicate`). Into a paused copy it
\* continues the group: the copy ignores the frames it holds and appends the rest.
SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].pub \in {"open", "fin", "reset"}
    /\ LET g == streams[i].g
       IN /\ acct' = acct \cup {g}
          /\ CASE Filling(g) \/ cached[g] = "open" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "held"]
                    /\ UNCHANGED <<cached, rec, have, malformed, reading>>
               [] cached[g] = "whole" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "dup"]
                    /\ UNCHANGED <<cached, rec, have, malformed, reading>>
               [] cached[g] = "paused" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "header"]
                    /\ cached' = [cached EXCEPT ![g] = "open"]
                    /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
                    /\ malformed' = (malformed \/ streams[i].start > have[g])
                    /\ reading' = IF Settled(g) THEN reading ELSE reading \cup {g}
                    /\ UNCHANGED have
               [] OTHER ->
                    /\ streams' = [streams EXCEPT ![i].sub = "header"]
                    /\ cached' = [cached EXCEPT ![g] = "open"]
                    /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
                    /\ have' = [have EXCEPT ![g] = streams[i].start]
                    /\ reading' = IF Settled(g) THEN reading ELSE reading \cup {g}
                    /\ UNCHANGED malformed
    /\ UNCHANGED <<pubVars, resp, sdrop, got>>

\* The stream filling the copy delivers its next frame, which may have arrived before a
\* reset that discards the rest. The copy ignores a frame it already holds.
SubFrameAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].read < streams[i].wrote
    /\ streams' = [streams EXCEPT ![i].read = @ + 1]
    /\ have' = [have EXCEPT ![streams[i].g] = Max(@, streams[i].start + streams[i].read + 1)]
    /\ UNCHANGED <<pubVars, resp, acct, reading, sdrop, rec, cached, got, malformed>>

\* The stream filling the copy is read to its FIN; once every byte arrived, a reset sent
\* after the FIN changes nothing. A held stream of the group then only duplicates it, a
\* continuation included.
SubFinAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].read = streams[i].wrote
    /\ \/ streams[i].pub \in {"fin", "acked"}
       \/ streams[i].pub = "reset" /\ streams[i].wasFin
    /\ LET g == streams[i].g
       IN /\ streams' = [j \in 1..Len(streams) |->
                            IF j = i THEN [streams[j] EXCEPT !.sub = "fin"]
                            ELSE IF j \in Held(g) THEN [streams[j] EXCEPT !.sub = "dup"]
                            ELSE streams[j]]
          /\ cached' = [cached EXCEPT ![g] = "whole"]
          /\ got' = got \cup {g}
          /\ reading' = reading \ {g}
    /\ UNCHANGED <<pubVars, resp, acct, sdrop, rec, have, malformed>>

\* The reset of the stream filling the copy arrives. Any code but YIELDED ends the group's
\* readers with its error, and the first held stream of the group opens it again as a fresh
\* copy (`TrackEntry::reopen`). YIELDED keeps the frames read: the first held stream
\* continues the group, or with none the copy is paused, and the tail takes back what the
\* paused stream's reading accounted, unless a drop or skip accounts for the group. A
\* YIELDED stream whose header arrived late, after another stream settled the group,
\* accounted nothing and takes nothing back.
SubResetAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].pub = "reset"
    /\ LET g == streams[i].g
           yielded == streams[i].err = "yielded"
           next == NextHeld(g)
           n == CHOOSE j \in next : TRUE
           pause == yielded /\ next = {} /\ cached[g] = "open"
           verdict == g \in sdrop \/ rec[g] \in {"final", "skip"}
           mine == g \in reading
       IN /\ streams' = [j \in 1..Len(streams) |->
                            IF j = i THEN [streams[j] EXCEPT !.sub = "reset"]
                            ELSE IF j \in next THEN [streams[j] EXCEPT !.sub = "header"]
                            ELSE streams[j]]
          /\ cached' = [cached EXCEPT ![g] = CASE next # {} -> "open"
                                              [] pause -> "paused"
                                              [] yielded -> @
                                              [] OTHER -> "aborted"]
          /\ have' = IF next # {} /\ ~yielded
                     THEN [have EXCEPT ![g] = streams[n].start]
                     ELSE have
          /\ malformed' = (malformed \/ (next # {} /\ yielded /\ streams[n].start > have[g]))
          /\ acct' = IF pause /\ mine /\ ~verdict THEN acct \ {g} ELSE acct
          /\ reading' = IF yielded /\ next # {} THEN reading ELSE reading \ {g}
    /\ UNCHANGED <<pubVars, resp, sdrop, rec, got>>

\* A held stream's own reset arrives.
SubResetHeldAt(i) ==
    /\ streams[i].sub = "held"
    /\ streams[i].pub = "reset"
    /\ streams' = [streams EXCEPT ![i].sub = "reset"]
    /\ UNCHANGED <<pubVars, resp, subVars>>

\* The copy's cache evicts a whole group.
Evict(g) ==
    /\ cached[g] = "whole"
    /\ cached' = [cached EXCEPT ![g] = "evicted"]
    /\ UNCHANGED <<pubVars, streams, resp, acct, reading, sdrop, rec, have, got, malformed>>

\* `apply_response` for SUBSCRIBE_DROP: the tail accounts for the group, and a paused copy
\* or a stream still reading it ends there (`TrackEntry::dropped`). A stale drop records
\* it in the copy unless the copy holds it (`give_up` keeps an open group, though not a
\* paused one), and a final drop records it and ends its readers unless the copy holds it
\* whole (`give_up` keeps a group a finished copy serves, the live one or a fetched head).
\* A fetched head still filling, which this model leaves out, holds the drop in its slot
\* until it finishes ("whole" here) or dies, when the drop is the slot's cut
\* (`Slot::verdict`): never lost, never beside a group.
OnDrop(m) ==
    LET g == m.g
    IN /\ acct' = acct \cup {g}
       /\ sdrop' = sdrop \cup {g}
       /\ IF m.err = "old"
          THEN /\ rec' = IF cached[g] \notin {"open", "whole"} /\ rec[g] # "skip"
                         THEN [rec EXCEPT ![g] = "stale"] ELSE rec
               /\ cached' = [cached EXCEPT ![g] = IF @ = "paused" THEN "aborted" ELSE @]
          ELSE /\ rec' = IF rec[g] = "skip" \/ cached[g] = "whole" THEN rec ELSE [rec EXCEPT ![g] = "final"]
               /\ cached' = [cached EXCEPT ![g] = IF @ \in {"open", "paused"} THEN "aborted" ELSE @]

\* `apply_response` for SUBSCRIBE_SKIP: final, overriding any drop, but a group that
\* arrived keeps it (`skip_received`).
OnSkip(m) ==
    LET g == m.g
    IN /\ acct' = acct \cup {g}
       /\ rec' = IF cached[g] \in {"open", "whole"} THEN rec ELSE [rec EXCEPT ![g] = "skip"]
       /\ UNCHANGED <<sdrop, cached>>

SubRecv ==
    /\ resp # <<>>
    /\ resp' = Tail(resp)
    /\ LET m == Head(resp)
       IN IF m.type = "drop" THEN OnDrop(m) ELSE OnSkip(m)
    /\ UNCHANGED <<pubVars, streams, reading, have, got, malformed>>

-----------------------------------------------------------------------------

\* A stream is done once both sides stopped: acknowledged, or reset with the header
\* read to the reset or lost.
StreamDone(s) ==
    \/ s.pub = "acked"
    \/ s.pub = "reset" /\ s.sub \in {"none", "fin", "reset", "dup"}

\* Nothing in flight and nothing left for the publisher to do.
Quiet ==
    /\ resp = <<>> /\ outq = <<>>
    /\ \A g \in Groups : cur[g] # "pending" /\ paused[g] = 0
    /\ \A i \in 1..Len(streams) : StreamDone(streams[i])

\* The end: quiet, or out of Group Streams to open.
Done ==
    /\ Quiet \/ Len(streams) = MaxStreams
    /\ UNCHANGED vars

\* Each action over the streams, named so a trace says which one ran.
StreamWrite == \E i \in 1..Len(streams) : StreamWriteAt(i)
StreamFin == \E i \in 1..Len(streams) : StreamFinAt(i)
StreamFail == \E i \in 1..Len(streams), err \in {"old", "evicted"} : StreamFailAt(i, err)
StreamYield == \E i \in 1..Len(streams) : StreamYieldAt(i)
StreamAck == \E i \in 1..Len(streams) : StreamAckAt(i)
SubHeader == \E i \in 1..Len(streams) : SubHeaderAt(i)
SubFrame == \E i \in 1..Len(streams) : SubFrameAt(i)
SubFin == \E i \in 1..Len(streams) : SubFinAt(i)
SubReset == \E i \in 1..Len(streams) : SubResetAt(i)
SubResetHeld == \E i \in 1..Len(streams) : SubResetHeldAt(i)

Next ==
    \/ \E g \in Groups : CursorHand(g)
    \/ \E g \in Groups, kind \in {"old", "evicted", "skip"} : CursorGiveUp(g, kind)
    \/ StreamWrite
    \/ StreamFin
    \/ StreamFail
    \/ StreamYield
    \/ StreamAck
    \/ \E g \in Groups : PubContinue(g)
    \/ \E g \in Groups, err \in {"old", "evicted"} : PausedGiveUp(g, err)
    \/ PubOutcomes
    \/ \E g \in Groups : PubFold(g)
    \/ SubHeader
    \/ SubFrame
    \/ SubFin
    \/ SubReset
    \/ SubResetHeld
    \/ \E g \in Groups : Evict(g)
    \/ SubRecv
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules                                                                   *)
-----------------------------------------------------------------------------

\* Once quiet, the tail accounts for every group: none is waited for until the grace.
QuietIsSettled == Quiet => acct = Groups

\* The publisher opens a second Group Stream for a group only to continue the group's
\* latest stream, which it reset with YIELDED.
ServedOnce ==
    \A i, j \in 1..Len(streams) :
        /\ (i < j /\ streams[i].g = streams[j].g) => streams[i].pub = "reset" /\ streams[j].of # 0
        /\ streams[j].of = i =>
              /\ i < j
              /\ streams[i].g = streams[j].g
              /\ streams[i].err = "yielded"
              /\ \A k \in i + 1..j - 1 : streams[k].g # streams[j].g

\* Every name is final: the publisher names a group in at most one drop, and in at most
\* one skip, which goes out over a drop of a group it never served and over nothing else.
NamedOnce ==
    \A g \in Groups :
        /\ ndrop[g] <= 1
        /\ nskip[g] <= 1
        /\ nskip[g] = 1 => \A i \in 1..Len(streams) : streams[i].g # g

\* A YIELDED reset alone never accounts for a group: while the publisher owes a paused
\* group its continuation or drop, the subscriber's paused copy stays owed. Only what else
\* accounts for a group excuses it: a drop or skip, a stream read to its FIN, or one whose
\* reset of another code arrived with its header.
PausedIsOwed ==
    \A g \in Groups :
        (/\ cached[g] = "paused"
         /\ paused[g] # 0
         /\ g \notin sdrop
         /\ rec[g] \notin {"final", "skip"}
         /\ g \notin got
         /\ \A i \in 1..Len(streams) :
                (streams[i].g = g /\ streams[i].sub = "reset") => streams[i].err = "yielded")
            => g \notin acct

\* A continuation never starts past the frames the paused copy holds.
ContinuesWithinHeld == ~malformed

\* The subscriber reads at most one Group Stream of a group at a time.
OneReader ==
    \A g \in Groups :
        Cardinality({i \in 1..Len(streams) : streams[i].g = g /\ streams[i].sub = "header"}) <= 1

=============================================================================
