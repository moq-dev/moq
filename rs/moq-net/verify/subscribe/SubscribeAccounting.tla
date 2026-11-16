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
(* A publisher out of stream credit may reset a Group Stream with YIELDED, *)
(* which pauses the group instead of accounting for it: the subscriber     *)
(* keeps the frames it read, and the publisher later continues the group   *)
(* on a new Group Stream from the reset stream's own Frame Start, or names *)
(* it in a drop. A continuation may overtake the YIELDED reset and waits,  *)
(* unread, for it.                                                         *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event. The track     *)
(* cursor's verdicts (hand a group out, give it up stale or final, skip    *)
(* it) and whether the cache still holds a fresh copy on an update are     *)
(* free choices, standing in for the cache, the clock, and the routes.     *)
(* So are when the publisher yields a stream, and whether it continues a   *)
(* paused group or gives it up, standing in for stream credit and expiry.  *)
(* Under `Pressure`, so is when the pool's debt folds what each side keeps *)
(* of the group: the publisher's record and the subscriber's tail.         *)
(*                                                                         *)
(* Scope: the subscription is already answered (SUBSCRIBE_OK) and covers   *)
(* every group; ranges, starts, fills, and the tail's grace are left out.  *)
(* GROUP_END takeover is left out: only the YIELDED reset pauses a group.  *)
(*                                                                         *)
(* With `Promote`, the subscription starts metadata-only and its first     *)
(* SUBSCRIBE_UPDATE asks for the payload again: the publisher serves each  *)
(* group it served as metadata again, drops it, or leaves it to its        *)
(* cursor, and the subscriber lets go of its metadata copies and owes      *)
(* them again. Each stream keeps the mode it opened in.                    *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Groups,      \* group sequences in range
    MaxUpdates,  \* SUBSCRIBE_UPDATEs the subscriber sends
    MaxStreams,  \* Group Streams the publisher opens
    MaxPauses,   \* YIELDED resets the publisher sends per group
    Frames,      \* frames in each group: 0 leaves frames out
    Pressure,    \* the cache pool may hand the readers debt, so each side folds
    Promote      \* metadata-only until the first update asks for the payload

\* A Group Stream: the publisher's side (`GroupState`), and the subscriber's.
\*   pub: "open" (serving), "fin" (FIN sent, awaiting its acknowledgement),
\*        "acked", or "reset" with `err` ("old", "evicted", or "yielded")
\*   sub: "none" (no header yet, or lost with a reset), "header" (filling the copy),
\*        "held" (waiting on the stream filling the copy), "fin", "reset", or "dup"
\*        "orphan" (a metadata stream read on once the subscription asks for the payload)
\*   of: the stream this one continues, or 0 for a fresh one
\*   meta: it carries each frame's header without its payload
\*   start: its Frame Start; wrote: frames the publisher wrote on it; read: frames the
\*        subscriber read from it
NewStream(g, of, start, meta) ==
    [g |-> g, pub |-> "open", err |-> "", wasFin |-> FALSE, sub |-> "none",
     of |-> of, start |-> start, wrote |-> 0, read |-> 0, meta |-> meta]

DropMsg(g, err, u) == [type |-> "drop", g |-> g, err |-> err, u |-> u]
SkipMsg(g) == [type |-> "skip", g |-> g, err |-> "", u |-> 0]

Max(a, b) == IF a > b THEN a ELSE b

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
    paused,     \* the YIELDED stream a paused group waits to continue, or 0
    pauses,     \* YIELDED resets sent for the group
    pmeta,      \* `meta`: the subscription asks for metadata only, as of the update applied
    metaRead,   \* the cursor's record of the groups it handed out as metadata (`note_meta`)
    pswept,     \* the groups that record let go once the cache no longer held them (`meta_span`)
    \* The wire.
    streams,    \* every Group Stream opened, in order
    resp,       \* the Subscribe Stream, publisher to subscriber
    ctrl,       \* the Subscribe Stream, subscriber to publisher: update numbers
    \* The subscriber's `SubStream`, `Tail`, and copy.
    sup,        \* `SubStream::update`: the latest number sent
    acct,       \* the tail's ledger: groups accounted for
    reading,    \* the part of `acct` only the stream filling the copy accounts for
    finished,   \* `Tail::finished`: groups whose stream finished since the last update
    forgot,     \* `Tail::folded`: the span a fold forgot since the last update
    sstale,     \* `SubStream::stale`: runs accounted by a stale drop under `sup`
    sdrop,      \* groups a stale drop under `sup` named, recorded or not
    rec,        \* the copy's record: "none", "stale", "final", or "skip"
    cached,     \* the copy's group: "none", "open", "paused", "whole", "aborted", "evicted"
    have,       \* the index after the last frame the copy holds of the group
    got,        \* history: the subscriber read the group to its FIN
    malformed,  \* a continuation started past the frames the copy holds
    violation,  \* the subscriber closed the session with a PROTOCOL_VIOLATION
    smeta,      \* `Tail::meta`: the subscription asks for metadata only, as of the update sent
    spromoted,  \* `SubStream::promoted`: the update that asked for the payload again, or 0
    metaCopy,   \* the copy's group holds headers only
    paid        \* history: the subscriber read the group's payload to its FIN

pubVars == <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered, outq, folded,
             paused, pauses, pmeta, metaRead, pswept>>
subVars == <<sup, acct, reading, finished, forgot, sstale, sdrop, rec, cached, have, got, malformed,
             violation, smeta, spromoted, metaCopy, paid>>
vars == <<pubVars, streams, resp, ctrl, subVars>>

Init ==
    /\ cur = [g \in Groups |-> "pending"]
    /\ late = [g \in Groups |-> FALSE]
    /\ pup = 0
    /\ served = {} /\ dropped = {} /\ dstale = {} /\ expired = {}
    /\ redrop = {} /\ reoffered = {} /\ outq = <<>> /\ folded = {}
    /\ paused = [g \in Groups |-> 0]
    /\ pauses = [g \in Groups |-> 0]
    /\ pmeta = Promote /\ metaRead = {} /\ pswept = {}
    /\ streams = <<>> /\ resp = <<>> /\ ctrl = <<>>
    /\ sup = 0
    /\ acct = {} /\ reading = {} /\ finished = {} /\ forgot = {} /\ sstale = {} /\ sdrop = {}
    /\ rec = [g \in Groups |-> "none"]
    /\ cached = [g \in Groups |-> "none"]
    /\ have = [g \in Groups |-> 0]
    /\ got = {}
    /\ malformed = FALSE
    /\ violation = FALSE
    /\ smeta = Promote
    /\ spromoted = 0
    /\ metaCopy = [g \in Groups |-> FALSE]
    /\ paid = {}

-----------------------------------------------------------------------------
(* Publisher                                                               *)
-----------------------------------------------------------------------------

\* `replace_stale`: a verdict or group replaces a drop of g.
ReplaceStale(g) ==
    /\ dropped' = dropped \ {g}
    /\ dstale' = dstale \ {g}
    /\ expired' = expired \ {g}
    /\ redrop' = redrop \ {g}

\* `serve`: open a Group Stream for g in the subscription's mode, which `mark_served`
\* records, and `note_meta` too when it carries metadata.
Serve(g) ==
    /\ Len(streams) < MaxStreams
    /\ streams' = Append(streams, NewStream(g, 0, 0, pmeta))
    /\ served' = served \cup {g}
    /\ metaRead' = IF pmeta THEN metaRead \cup {g} ELSE metaRead
    /\ pswept' = pswept

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
            THEN UNCHANGED <<served, dropped, dstale, expired, redrop, streams, metaRead, pswept>>
            ELSE /\ Serve(g)
                 /\ UNCHANGED <<dropped, dstale, expired, redrop>>
    /\ UNCHANGED <<pup, reoffered, outq, folded, paused, pauses, pmeta, resp, ctrl, subVars>>

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
    /\ UNCHANGED <<late, pup, reoffered, outq, folded, paused, pauses, pmeta, metaRead, pswept, streams, ctrl,
                   subVars>>

\* A serving stream writes its next frame.
StreamWriteAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams[i].start + streams[i].wrote < Frames
    /\ streams' = [streams EXCEPT ![i].wrote = @ + 1]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* A serving stream writes its last frame and FINs (`GroupState::Closed`).
StreamFinAt(i) ==
    /\ streams[i].pub = "open"
    /\ streams[i].start + streams[i].wrote = Frames
    /\ streams' = [streams EXCEPT ![i].pub = "fin"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* A stream fails and resets: it expires stale while serving or while its FIN awaits
\* the acknowledgement, or is evicted while serving. `settle_failed` queues the outcome.
StreamFailAt(i, err) ==
    /\ \/ streams[i].pub = "open"
       \/ streams[i].pub = "fin" /\ err = "old"
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = err,
                                  ![i].wasFin = (streams[i].pub = "fin")]
    /\ outq' = Append(outq, <<streams[i].g, err, streams[i].meta>>)
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered,
                   folded, paused, pauses, pmeta, metaRead, pswept, resp, ctrl, subVars>>

\* Out of stream credit, the publisher resets a serving stream with YIELDED. The group is
\* paused, not given up: nothing names it, and the publisher owes a continuation or a drop.
\* A metadata stream yielded once the payload is asked for again pauses nothing: the
\* promotion owes its group the payload or a drop.
StreamYieldAt(i) ==
    /\ streams[i].pub = "open"
    /\ pauses[streams[i].g] < MaxPauses
    /\ streams' = [streams EXCEPT ![i].pub = "reset", ![i].err = "yielded"]
    /\ paused' = IF streams[i].meta /\ ~pmeta THEN paused ELSE [paused EXCEPT ![streams[i].g] = i]
    /\ pauses' = [pauses EXCEPT ![streams[i].g] = @ + 1]
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered,
                   outq, folded, pmeta, metaRead, pswept, resp, ctrl, subVars>>

\* The publisher continues a paused group on a new Group Stream, from the YIELDED
\* stream's own Frame Start: a reset can discard frames the transport acknowledged.
PubContinue(g) ==
    /\ paused[g] # 0
    /\ Len(streams) < MaxStreams
    /\ LET k == paused[g]
       IN streams' = Append(streams, NewStream(g, k, streams[k].start, streams[k].meta))
    /\ paused' = [paused EXCEPT ![g] = 0]
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered,
                   outq, folded, pauses, pmeta, metaRead, pswept, resp, ctrl, subVars>>

\* The publisher gives a paused group up instead, as it would the stream it yielded:
\* it expires stale under the delivery budget, or is evicted.
PausedGiveUp(g, err) ==
    /\ paused[g] # 0
    /\ paused' = [paused EXCEPT ![g] = 0]
    /\ outq' = Append(outq, <<g, err, streams[paused[g]].meta>>)
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered,
                   folded, pauses, pmeta, metaRead, pswept, streams, resp, ctrl, subVars>>

\* The peer acknowledged everything up to the FIN.
StreamAckAt(i) ==
    /\ streams[i].pub = "fin"
    /\ streams[i].sub \in {"fin", "dup"}
    /\ streams' = [streams EXCEPT ![i].pub = "acked"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* `poll_step` names the failed streams: a stale expiry is a stale drop the next
\* update judges again, anything else a final one. A stale expiry of a group the
\* record folded while its stream was open is final too: no update re-offers it. A
\* metadata stream that failed once the subscription asks for the payload is named in
\* no drop: the promotion owes its group the payload or a drop of its own.
PubOutcomes ==
    /\ outq # <<>>
    /\ LET g == Head(outq)[1]
           err == IF Head(outq)[2] = "old" /\ g \in folded THEN "evicted" ELSE Head(outq)[2]
           promoted == Head(outq)[3] /\ ~pmeta
       IN /\ outq' = Tail(outq)
          /\ resp' = IF promoted THEN resp ELSE Append(resp, DropMsg(g, err, pup))
          /\ IF err = "old" /\ ~promoted
             THEN /\ expired' = expired \cup {g}
                  /\ dropped' = dropped \cup {g}
                  /\ dstale' = dstale \cup {g}
             ELSE UNCHANGED <<expired, dropped, dstale>>
    /\ UNCHANGED <<cur, late, pup, served, redrop, reoffered, folded, paused, pauses, pmeta, metaRead, pswept,
                   streams, ctrl, subVars>>

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
    /\ UNCHANGED <<cur, late, pup, reoffered, outq, folded, paused, pauses, pmeta, metaRead, pswept, streams,
                   ctrl, subVars>>

\* `poll_track` serves a re-offered group, unless a verdict since accounted for it. One
\* the record folded while it waited is named in a final drop instead.
PubReoffer ==
    /\ outq = <<>> /\ redrop = {}
    /\ reoffered # {}
    /\ LET g == CHOOSE x \in reoffered : \A y \in reoffered : x <= y
       IN /\ reoffered' = reoffered \ {g}
          /\ CASE g \in folded ->
                    /\ resp' = Append(resp, DropMsg(g, "evicted", pup))
                    /\ UNCHANGED <<served, streams, metaRead, pswept>>
               [] g \in served -> UNCHANGED <<served, streams, resp, metaRead, pswept>>
               [] OTHER -> Serve(g) /\ UNCHANGED resp
    /\ UNCHANGED <<cur, late, pup, dropped, dstale, expired, redrop, outq, folded, paused,
                   pauses, pmeta, ctrl, subVars>>

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
                   paused, pauses, pmeta, metaRead, pswept, streams, resp, ctrl, subVars>>

\* `note_meta` sweeps the record of the groups handed out as metadata once its runs double:
\* it lets go of g once the cache no longer holds it, which no later copy brings back.
PubSweep(g) ==
    /\ g \in metaRead
    /\ metaRead' = metaRead \ {g}
    /\ pswept' = pswept \cup {g}
    /\ UNCHANGED <<cur, late, pup, served, dropped, dstale, expired, redrop, reoffered, outq, folded,
                   paused, pauses, pmeta, streams, resp, ctrl, subVars>>

\* `TrackRun::update` applies a SUBSCRIBE_UPDATE: the cursor's stale verdicts reopen
\* (`reoffer_stale`, then `owe`), and each expired stream is judged again: served
\* again where the cache still holds a fresh copy (`reoffer_groups`), else dropped again.
\* A paused group is still owed its continuation or drop, so the update leaves it be.
\*
\* The update asking for the payload again (`update_from`) first hands out again each
\* group the cursor handed out as metadata (`promote_from`): the payload copies the cache
\* holds fresh (`repay`) are re-offered, those no copy is left of are dropped again, and
\* the cursor goes back for the rest. Each one served by a stream is served no longer
\* (`unserve`), and a metadata group paused is let go. The stale judgment above then
\* judges the re-offered and dropped ones again.
PubUpdate ==
    /\ ctrl # <<>>
    /\ LET u == Head(ctrl)
           promote == pmeta /\ u = 1
           \* The record's sweep let go of a group once the cache no longer held it: the
           \* promotion still gives it up (`meta_span`), as no copy is left to serve.
           promoted == IF promote THEN metaRead \cup pswept ELSE {}
           reopened == {g \in Groups : cur[g] = "stale"}
       IN /\ ctrl' = Tail(ctrl)
          /\ pup' = u
          /\ pmeta' = (pmeta /\ ~promote)
          /\ metaRead' = metaRead \ promoted
          /\ pswept' = IF promote THEN {} ELSE pswept
          /\ paused' = [g \in Groups |-> IF promote /\ paused[g] # 0 /\ streams[paused[g]].meta
                                        THEN 0 ELSE paused[g]]
          /\ \E held \in SUBSET promoted, gone \in SUBSET promoted :
                LET rest == promoted \ (held \cup gone)
                    judged == expired \cup reoffered \cup redrop \cup held \cup gone
                IN /\ held \cap gone = {}
                   /\ pswept \cap promoted \subseteq gone
                   /\ cur' = [g \in Groups |-> IF g \in reopened \cup rest THEN "pending" ELSE cur[g]]
                   \* One cache answers both: the stale judgment re-offers what the promotion
                   \* found cached fresh, and nothing it found uncached.
                   /\ \E fresh \in SUBSET judged :
                         LET owed == reopened \cup fresh
                         IN /\ held \subseteq fresh
                            /\ fresh \cap (rest \cup gone) = {}
                            /\ reoffered' = fresh
                            /\ redrop' = judged \ fresh
                            /\ expired' = {}
                            /\ served' = served \ (owed \cup (promoted \ dropped))
                            /\ dropped' = dropped \ owed
                            /\ dstale' = dstale \ owed
    /\ UNCHANGED <<late, outq, folded, pauses, streams, resp, subVars>>

-----------------------------------------------------------------------------
(* Subscriber                                                              *)
-----------------------------------------------------------------------------

\* The streams of g waiting, unread, for the copy another stream is filling.
Held(g) == {i \in 1..Len(streams) : streams[i].g = g /\ streams[i].sub = "held"}

\* A stream of g is still being read into the copy, even one a final drop aborted.
Filling(g) == \E j \in 1..Len(streams) : streams[j].g = g /\ streams[j].sub = "header"

\* The ledger already accounts for g, not only through a stream still reading it, and the
\* next update does not take that back (`buffer_update` owes again only a stale run the
\* copy still records): a stream that finished it, a reset, a final drop, a skip, or a stale
\* drop a fold marked overtaken. A stream that starts filling the copy then adds nothing a
\* YIELDED reset of it could take back.
Settled(g) == g \in acct /\ g \notin reading /\ ~(g \in sstale /\ rec[g] = "stale")

\* The first of them, which takes the copy over once the filling stream ends.
NextHeld(g) == {j \in Held(g) : \A k \in Held(g) : j <= k}

\* A Group Stream's header arrives (`TrackEntry::open`, `Reading::open`): the tail
\* accounts for the group. It fills the copy and replaces a stale record of it, unless
\* another stream of it is still read (`Open::Duplicate`, held until that one settles)
\* or the copy holds the group whole (`Error::Duplicate`). Into a paused copy it
\* continues the group: the copy ignores the frames it holds and appends the rest.
\* A metadata stream whose header arrives once the subscription asks for the payload is
\* read on without accounting for anything: the update owes the group's payload or a drop.
SubLateMeta(i) ==
    /\ streams' = [streams EXCEPT ![i].sub = "orphan"]
    /\ UNCHANGED <<acct, cached, rec, have, malformed, reading, metaCopy>>

SubHeaderAt(i) ==
    /\ streams[i].sub = "none"
    /\ streams[i].pub \in {"open", "fin", "reset"}
    /\ LET g == streams[i].g
       IN IF streams[i].meta /\ ~smeta THEN SubLateMeta(i) ELSE
          /\ acct' = acct \cup {g}
          /\ CASE Filling(g) \/ cached[g] = "open" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "held"]
                    /\ UNCHANGED <<cached, rec, have, malformed, reading, metaCopy>>
               [] cached[g] = "whole" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "dup"]
                    /\ UNCHANGED <<cached, rec, have, malformed, reading, metaCopy>>
               [] cached[g] = "paused" ->
                    /\ streams' = [streams EXCEPT ![i].sub = "header"]
                    /\ cached' = [cached EXCEPT ![g] = "open"]
                    /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
                    /\ malformed' = (malformed \/ streams[i].start > have[g])
                    /\ reading' = IF Settled(g) THEN reading ELSE reading \cup {g}
                    /\ UNCHANGED <<have, metaCopy>>
               [] OTHER ->
                    /\ streams' = [streams EXCEPT ![i].sub = "header"]
                    /\ cached' = [cached EXCEPT ![g] = "open"]
                    /\ rec' = [rec EXCEPT ![g] = IF @ = "stale" THEN "none" ELSE @]
                    /\ have' = [have EXCEPT ![g] = streams[i].start]
                    /\ reading' = IF Settled(g) THEN reading ELSE reading \cup {g}
                    /\ metaCopy' = [metaCopy EXCEPT ![g] = streams[i].meta]
                    /\ UNCHANGED malformed
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, finished, forgot, sstale, sdrop, got, violation, smeta, spromoted,
                   paid>>

\* The stream filling the copy delivers its next frame, which may have arrived before a
\* reset that discards the rest. The copy ignores a frame it already holds.
SubFrameAt(i) ==
    /\ streams[i].sub = "header"
    /\ streams[i].read < streams[i].wrote
    /\ streams' = [streams EXCEPT ![i].read = @ + 1]
    /\ have' = [have EXCEPT ![streams[i].g] = Max(@, streams[i].start + streams[i].read + 1)]
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, acct, reading, finished, forgot, sstale, sdrop, rec, cached,
                   got, malformed, violation, smeta, spromoted, metaCopy, paid>>

\* The stream filling the copy is read to its FIN (`Reading::finish`); once every byte
\* arrived, a reset sent after the FIN changes nothing. A held stream of the group then
\* only duplicates it, a continuation included.
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
          /\ finished' = finished \cup {g}
          /\ cached' = [cached EXCEPT ![g] = "whole"]
          /\ got' = got \cup {g}
          /\ paid' = IF streams[i].meta THEN paid ELSE paid \cup {g}
          /\ reading' = reading \ {g}
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, acct, forgot, sstale, sdrop, rec, have, malformed,
                   violation, smeta, spromoted, metaCopy>>

\* The reset of the stream filling the copy arrives. Any code but YIELDED ends the group's
\* readers with its error, and the first held stream of the group opens it again as a fresh
\* copy (`TrackEntry::reopen`). YIELDED keeps the frames read: the first held stream
\* continues the group, or with none the copy is paused, and the tail takes back what the
\* paused stream's reading accounted, unless a drop or skip under the update in effect
\* accounts for the group. A stale drop the filling stream had won over then records the
\* group stale, as it would arriving now. A YIELDED stream whose header arrived late, after
\* another stream settled the group, accounted nothing and takes nothing back.
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
           restale == pause /\ mine /\ g \in sdrop /\ rec[g] = "none"
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
          /\ rec' = IF restale THEN [rec EXCEPT ![g] = "stale"] ELSE rec
          /\ sstale' = IF restale THEN sstale \cup {g} ELSE sstale
    /\ UNCHANGED <<pubVars, resp, ctrl, sup, finished, forgot, sdrop, got, violation, smeta, spromoted, metaCopy,
                   paid>>

\* A held stream's own reset arrives.
SubResetHeldAt(i) ==
    /\ streams[i].sub = "held"
    /\ streams[i].pub = "reset"
    /\ streams' = [streams EXCEPT ![i].sub = "reset"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* A metadata stream read on after the subscription asked for the payload ends, by its FIN
\* or its reset: it fills no copy, and its FIN finishes nothing (`Reading::finish`).
SubOrphanEndAt(i) ==
    /\ streams[i].sub = "orphan"
    /\ streams[i].pub \in {"fin", "acked", "reset"}
    /\ streams' = [streams EXCEPT ![i].sub = IF streams[i].pub = "reset" THEN "reset" ELSE "fin"]
    /\ UNCHANGED <<pubVars, resp, ctrl, subVars>>

\* The copy's cache evicts a whole group.
Evict(g) ==
    /\ cached[g] = "whole"
    /\ cached' = [cached EXCEPT ![g] = "evicted"]
    /\ UNCHANGED <<pubVars, streams, resp, ctrl, sup, acct, reading, finished, forgot, sstale,
                   sdrop, rec, have, got, malformed, violation, smeta, spromoted, metaCopy, paid>>

\* The pool hands the tail debt and `Tail::rebill` folds every finished run but the newest
\* into the span it forgot: one group stands for any of them, since a later group may have
\* finished, and the span takes in the gaps between them, so g need not have finished or
\* even arrived.
TailFold(g) ==
    /\ Pressure
    /\ g \notin forgot
    /\ finished' = finished \ {g}
    /\ forgot' = forgot \cup {g}
    /\ UNCHANGED <<pubVars, streams, resp, ctrl, sup, acct, reading, sstale, sdrop, rec, cached,
                   have, got, malformed, violation, smeta, spromoted, metaCopy, paid>>

\* `apply_response` for SUBSCRIBE_DROP. A stale drop under a replaced update is no
\* verdict. Otherwise the tail accounts for the group; a stale drop records it in the
\* copy unless its stream finished under this update or the copy holds it
\* (`give_up` keeps an open group, though not a paused one), and a final drop records it
\* and ends its readers unless the copy holds it whole (`give_up` keeps a group a finished
\* copy serves, the live one or a fetched head). A fetched head still filling, which this
\* model leaves out, holds the drop in its slot until it finishes ("whole" here) or dies,
\* when the drop is the slot's cut (`Slot::verdict`): never lost, never beside a group.
\* A stale drop of a group inside the span a fold forgot that the ledger already
\* accounted (`Tail::overtaken`) may be one a finished stream overtook, so the next
\* update owes it nothing again.
OnDrop(m) ==
    LET g == m.g
        stale == m.err = "old"
    IN IF m.u > sup
       THEN /\ violation' = TRUE
            /\ UNCHANGED <<acct, sstale, sdrop, rec, cached>>
       ELSE IF stale /\ m.u < sup
       THEN UNCHANGED <<acct, sstale, sdrop, rec, cached, violation>>
       ELSE IF stale
       THEN LET recorded == g \notin finished
                            /\ (cached[g] \notin {"open", "whole"} \/ (metaCopy[g] /\ ~smeta))
                            /\ rec[g] # "skip"
                owed == recorded /\ ~(g \in forgot /\ g \in acct)
            IN /\ acct' = acct \cup {g}
               /\ rec' = IF recorded THEN [rec EXCEPT ![g] = "stale"] ELSE rec
               /\ sstale' = IF owed THEN sstale \cup {g} ELSE sstale
               /\ sdrop' = sdrop \cup {g}
               /\ UNCHANGED <<cached, violation>>
       ELSE LET \* Sent before the update asking for the payload again, it named a metadata
                \* stream: a payload copy arriving since wins over it (`give_up_as`), and
                \* the drop accounts only for what it records.
                wins == m.u < spromoted /\ ~metaCopy[g] /\ cached[g] \in {"open", "paused"}
            IN /\ acct' = IF wins THEN acct ELSE acct \cup {g}
               /\ sstale' = sstale \ {g}
               /\ rec' = IF rec[g] = "skip" \/ cached[g] = "whole" \/ wins THEN rec
                         ELSE [rec EXCEPT ![g] = "final"]
               /\ cached' = [cached EXCEPT ![g] = IF @ \in {"open", "paused"} /\ ~wins THEN "aborted" ELSE @]
               /\ UNCHANGED <<sdrop, violation>>

\* `apply_response` for SUBSCRIBE_SKIP: final, overriding any drop, but a group that
\* arrived keeps it (`skip_received`).
OnSkip(m) ==
    LET g == m.g
    IN /\ acct' = acct \cup {g}
       /\ sstale' = sstale \ {g}
       /\ rec' = IF cached[g] \in {"open", "whole"} THEN rec ELSE [rec EXCEPT ![g] = "skip"]
       /\ UNCHANGED <<sdrop, cached, violation>>

SubRecv ==
    /\ resp # <<>>
    /\ resp' = Tail(resp)
    /\ LET m == Head(resp)
       IN IF m.type = "drop" THEN OnDrop(m) ELSE OnSkip(m)
    /\ UNCHANGED <<pubVars, streams, ctrl, sup, reading, finished, forgot, have, got, malformed, smeta, spromoted,
                   metaCopy, paid>>

\* `buffer_update`: the copy forgets every stale record (`supersede_stale`), the tail
\* owes again each stale run this subscription accounted that the copy still held,
\* and `Tail::updated` forgets which streams finished and what a fold forgot.
\*
\* The first update asks for the payload again under `Promote` (`set_meta`): the copy lets
\* go of its metadata copies (`track::Producer::promote`), and the tail owes them again,
\* save one a drop or reset ended, which that verdict accounted. A metadata stream still
\* filling one, or held behind one, is read on into nothing.
SubUpdate ==
    /\ sup < MaxUpdates
    /\ ~violation
    /\ LET held == {g \in Groups : rec[g] = "stale"}
           promote == smeta
           let == {g \in Groups : promote /\ metaCopy[g] /\ cached[g] \in {"open", "whole", "paused"}}
       IN /\ sup' = sup + 1
          /\ ctrl' = Append(ctrl, sup + 1)
          /\ smeta' = FALSE
          /\ spromoted' = IF promote THEN sup + 1 ELSE spromoted
          /\ rec' = [g \in Groups |-> IF rec[g] = "stale" THEN "none" ELSE rec[g]]
          /\ acct' = (acct \ (sstale \cap held)) \ let
          /\ reading' = reading \ let
          /\ cached' = [g \in Groups |-> IF g \in let THEN "none" ELSE cached[g]]
          /\ have' = [g \in Groups |-> IF g \in let THEN 0 ELSE have[g]]
          /\ metaCopy' = [g \in Groups |-> metaCopy[g] /\ g \notin let]
          /\ streams' = [j \in 1..Len(streams) |->
                            IF promote /\ streams[j].meta /\ streams[j].sub \in {"header", "held"}
                            THEN [streams[j] EXCEPT !.sub = "orphan"]
                            ELSE streams[j]]
          /\ sstale' = {}
          /\ sdrop' = {}
          /\ finished' = {}
          /\ forgot' = {}
    /\ UNCHANGED <<pubVars, resp, got, malformed, violation, paid>>

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
SubOrphanEnd == \E i \in 1..Len(streams) : SubOrphanEndAt(i)

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
    \/ PubRedrop
    \/ PubReoffer
    \/ PubUpdate
    \/ \E g \in Groups : PubSweep(g)
    \/ \E g \in Groups : PubFold(g)
    \/ SubHeader
    \/ SubFrame
    \/ SubFin
    \/ SubReset
    \/ SubResetHeld
    \/ SubOrphanEnd
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

\* The publisher serves a group again only to re-offer one whose stream it reset, to
\* continue the latest stream of the group, which it reset with YIELDED, or to serve the
\* payload of a group it served as metadata.
ServedOnce ==
    \A i, j \in 1..Len(streams) :
        /\ (i < j /\ streams[i].g = streams[j].g) =>
              streams[i].pub = "reset" \/ (streams[i].meta /\ ~streams[j].meta)
        /\ streams[j].of = i =>
              /\ i < j
              /\ streams[i].g = streams[j].g
              /\ streams[i].err = "yielded"
              /\ \A k \in i + 1..j - 1 : streams[k].g # streams[j].g

\* A YIELDED reset alone never accounts for a group: while the publisher owes a paused
\* group its continuation or drop, the subscriber's paused copy stays owed. Only what else
\* accounts for a group excuses it: a drop or skip under the update in effect, a stream
\* read to its FIN, or one whose reset of another code arrived with its header.
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

\* A drop never names an update the subscriber has not sent.
NoViolation == ~violation

\* Once quiet after asking for the payload again, the subscriber's copy holds each group's
\* payload copy, whatever became of it, or records the group given up: none is left with
\* nothing, or a metadata copy, for a payload reader to wait on until the grace. That
\* holds for a group the publisher's record of what it served as metadata let go too.
PaidOnPromotion ==
    (Quiet /\ ~smeta /\ ~pmeta) =>
        \A g \in Groups : g \in paid \/ rec[g] # "none" \/ (cached[g] # "none" /\ ~metaCopy[g])

=============================================================================
