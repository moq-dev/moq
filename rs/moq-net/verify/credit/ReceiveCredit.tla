---------------------------- MODULE ReceiveCredit ----------------------------
(***************************************************************************)
(* One connection's receive credit, between moq-quic's leases and stuck-   *)
(* window eviction (rs/moq-quic/src/connection/streams/lease.rs) and       *)
(* moq-net's ingest, which paces each group stream by its group's fastest  *)
(* reader (rs/moq-net/src/coding/reader.rs, transport/boxed.rs,            *)
(* model/pace.rs).                                                         *)
(*                                                                         *)
(* Bytes are units. The peer sends a unit on a stream once its stream and  *)
(* connection credit allow; it arrives at once and in order, so no gap     *)
(* holds a reader. Every stream carries one group of `Frames` frames of    *)
(* `FrameLen` units, sealed every `Seg` units. A reader leases what        *)
(* arrived, takes one chunk of it (at most to the next seal, or the whole  *)
(* rest of the frame), and hands the lease back: released at a seal, which *)
(* returns the credit of every unit taken, restored short of one, which    *)
(* keeps it. Credit returns to the peer at once.                           *)
(*                                                                         *)
(* Ingest checks its group's pace gate at every seal: the writer may run   *)
(* at most the pace window past the group's fastest reader. Past it, the   *)
(* stream pauses: it reserves the credit it holds from the session's       *)
(* paused budget, half the connection window, and is not read until the   *)
(* reader catches up, the reader leaves, or the stream is evicted. Refused *)
(* a reservation, it reads on. Ingest shapes each stream's window to the   *)
(* room its group has left plus the allowance of the stream's slot.        *)
(*                                                                         *)
(* When the peer has no connection credit left, moq-quic evicts the lowest *)
(* ranked stream holding credit, unless an evicted reader has yet to stop  *)
(* its stream or some reader has data it has not looked at.                *)
(*                                                                         *)
(* Each step is one poll of the code that handles one event, under the     *)
(* connection's lock where the code takes it. A task parks until an event  *)
(* it registered for wakes it.                                             *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    N,          \* streams, numbered in the order the peer opens them
    Frames,     \* frames per stream
    FrameLen,   \* units per frame
    Seg,        \* units per segment: a frame's credit returns at each seal
    W,          \* the connection receive window
    SW,         \* the stream receive window
    Ahead,      \* `pace::AHEAD`, the most a writer runs ahead of its fastest reader
    Late,       \* a group's first reader may arrive after its writer started
    Leave       \* a group's last reader may leave it

ASSUME FrameLen % Seg = 0 /\ N >= 1 /\ Frames >= 1 /\ Seg >= 1 /\ SW >= 1 /\ W >= 1

Streams == 1..N
None == -1
Min(a, b) == IF a < b THEN a ELSE b
Max(a, b) == IF a > b THEN a ELSE b

RECURSIVE SumTo(_, _)
SumTo(f(_), n) == IF n = 0 THEN 0 ELSE f(n) + SumTo(f, n - 1)

Len == Frames * FrameLen

\* `Budgeted::window` on a transport that shapes credit: `pace::window`, the stream window
\* capped at AHEAD and the connection window, plus a stream window, capped at AHEAD.
PaceWindow == Min(Min(Min(SW, Ahead), W) + SW, Ahead)

\* `Pacing::cap`: what the session's paused streams may hold together.
Cap == W \div 2

\* `Pacing::allowance`: the credit a stream in `slot` may hold past its group's room.
Allowance(slot) == Min(Cap \div ((slot + 1) * (slot + 2)), SW)

\* `StreamsState::ranks`: the lowest priority first, then the newest. Every stream ranks at one
\* priority here, so the newest goes first.
Order == [i \in Streams |-> N + 1 - i]

VARIABLES
    \* moq-quic, one record per receive stream (`Recv` and its `Leased`)
    q,
    \* moq-quic, the connection: credit returned to the peer, and `stuck_waiting`
    credited,
    stuckWaiting,
    \* moq-net ingest, per stream: where its task is, and whether a wake is pending
    pc,         \* "new" (not accepted), "loop" (reading), "out" (a lease in hand), "write"
                \* (the chunk not yet in the frame), "paused", or "done" (stopped)
    woken,
    charge,     \* the paused credit it reserved (`Budgeted::held`), or None
    slot,       \* its slot among the session's shaped streams, or None
    \* the group
    wr,         \* units its writer wrote
    rd,         \* where the last frame its fastest reader took ends
    held,       \* whether a reader holds it: "late" (none yet), "held", or "gone"
    \* history
    refused     \* some pause was refused

vars == <<q, credited, stuckWaiting, pc, woken, charge, slot, wr, rd, held, refused>>

(***************************************************************************)
(* A receive stream in moq-quic.                                           *)
(*   arr:    units arrived (the stream's `end`)                            *)
(*   filled: units moved into the lease (`assembler.bytes_read`)           *)
(*   taken:  units the reader took and handed the lease back over          *)
(*   rel:    units whose credit the reader released                        *)
(*   hand:   units taken from a lease still out                            *)
(*   msd:    the stream limit the peer was sent (`sent_max_stream_data`)   *)
(*   due:    a `MAX_STREAM_DATA` is queued (`pending.max_stream_data`)     *)
(*   win:    the window `set_window` last set, SW until then               *)
(*   want, ready, leased (`filled`), ranked, unl (in `unlooked`), out:     *)
(*           `Leased`'s flags                                              *)
(*   rdr:    "reading", "evicted", or "stopped" (`Reader`)                 *)
(***************************************************************************)
Fresh == [arr |-> 0, filled |-> 0, taken |-> 0, rel |-> 0, hand |-> 0, msd |-> SW, due |-> FALSE,
          win |-> SW, want |-> FALSE, ready |-> FALSE, leased |-> FALSE, ranked |-> FALSE,
          unl |-> FALSE, out |-> FALSE, rdr |-> "reading"]

Unread(r) == r.arr - r.filled

\* `Leased::held`: leased units not yet released, taken or not.
Held(r) == r.filled - r.rel

\* The connection credit a stream withholds from the peer.
Hold(r) == CASE r.rdr = "reading" -> Unread(r) + Held(r)
             [] r.rdr = "evicted" -> Held(r)
             [] r.rdr = "stopped" -> 0

DataRecvd(qq) == SumTo(LAMBDA s: qq[s].arr, N)
EvictedHeld(qq) == SumTo(LAMBDA s: IF qq[s].rdr = "evicted" THEN Held(qq[s]) ELSE 0, N)
\* The peer has no connection credit left.
Stuck(qq, cr) == W + cr <= DataRecvd(qq)

\* `unlooked`: a reader that may progress without new credit.
Unlooked(r) == r.rdr = "reading"
    /\ ((Unread(r) > 0 /\ (~r.leased \/ r.want)) \/ r.ready \/ r.out)

\* `Recv::can_send_flow_control`: the FIN, which rides the last unit, has not arrived.
CanGrant(r) == r.arr < Len /\ r.rdr = "reading"

\* `Recv::max_stream_data`: credit runs from what the reader took, a window past it, or the
\* unit a waiting reader wants.
Limit(r) == Max(r.taken + Max(Min(r.win, SW), IF r.want THEN 1 ELSE 0), r.msd)

\* `queue_max_stream_data`: queue a `MAX_STREAM_DATA` once the limit grew. The packet that
\* carries it computes the limit again (`Transmit`).
Queue(r) == IF CanGrant(r) /\ Limit(r) > r.msd THEN [r EXCEPT !.due = TRUE] ELSE r

\* `Recv::credit`: what arrived and was not released, and what the peer
\* may still send; capped at the final size once the FIN arrived.
Credit(r) == IF r.rdr = "reading"
             THEN (IF r.arr = Len THEN Min(Limit(r), Len) ELSE Limit(r)) - r.rel
             ELSE Held(r)

(***************************************************************************)
(* `StreamsState::evict_if_stuck`, one atomic pass under the lock. It      *)
(* returns every outcome the order of `unlooked`, a hash set, allows: the  *)
(* new streams, credit returned, `stuck_waiting`, and the streams woken.   *)
(***************************************************************************)
Evict(qq, s) ==
    [qq EXCEPT ![s].filled = qq[s].taken, ![s].want = FALSE, ![s].ready = FALSE,
               ![s].rdr = "evicted", ![s].unl = FALSE]

RECURSIVE EvictLoop(_, _, _, _)
EvictLoop(qq, cr, woke, i) ==
    IF i > N \/ ~Stuck(qq, cr) \/ EvictedHeld(qq) > 0
    THEN [q |-> qq, cr |-> cr, woke |-> woke]
    ELSE LET s == Order[i] IN
         IF ~qq[s].ranked THEN EvictLoop(qq, cr, woke, i + 1)
         ELSE LET q1 == [qq EXCEPT ![s].ranked = FALSE] IN
              IF q1[s].rdr # "reading" \/ Hold(q1[s]) = 0
              THEN EvictLoop(q1, cr, woke, i)
              \* `evict`'s debug assertion: the trim found no such reader.
              ELSE IF Assert(~Unlooked(q1[s]), <<"evicted a stream whose reader may progress", s>>)
              THEN EvictLoop(Evict(q1, s), cr + q1[s].arr - q1[s].taken, woke \cup {s}, i)
              ELSE [q |-> qq, cr |-> cr, woke |-> woke]

Pass(qq, cr) ==
    LET found == {s \in Streams : qq[s].unl /\ Unlooked(qq[s])} IN
    IF ~Stuck(qq, cr) \/ EvictedHeld(qq) > 0
    THEN {[q |-> qq, cr |-> cr, sw |-> FALSE, woke |-> {}]}
    ELSE IF found # {}
    THEN {[q |-> qq, cr |-> cr, sw |-> qq[s].out, woke |-> {}] : s \in found}
    ELSE LET trimmed == [s \in Streams |-> [qq[s] EXCEPT !.unl = FALSE]]
             e == EvictLoop(trimmed, cr, {}, 1)
         IN {[q |-> e.q, cr |-> e.cr, sw |-> FALSE, woke |-> e.woke]}

\* No pass: the books as `qq` and `cr` leave them.
Unpassed(qq, cr) == {[q |-> qq, cr |-> cr, sw |-> stuckWaiting, woke |-> {}]}

\* Wake the tasks parked on a stream's reads (`Wait::Read`): a lease waiting for data, or a
\* paused stream watching for a reset.
Wake(w, ss) == [s \in Streams |-> w[s] \/ (s \in ss /\ pc[s] \in {"loop", "paused"})]

\* Apply a pass's outcome `p` on top of a step that leaves `w` as the wakes.
Settle(p, w) ==
    /\ q' = p.q
    /\ credited' = p.cr
    /\ stuckWaiting' = p.sw
    /\ woken' = Wake(w, p.woke)

(***************************************************************************)
(* moq-net ingest.                                                         *)
(***************************************************************************)
\* The slot `Slots::take` hands out: the lowest no live stream holds.
LowestFree(sl) == CHOOSE i \in 0..N : i \notin {sl[t] : t \in Streams}
                                     /\ \A j \in 0..(i - 1) : j \in {sl[t] : t \in Streams}

SlotOf(s) == IF slot[s] = None THEN LowestFree(slot) ELSE slot[s]

PausedTotal(ch) == SumTo(LAMBDA s: IF ch[s] = None THEN 0 ELSE ch[s], N)

\* `Pace::room`, then `saturating_sub(frame)`: what the writer may still take, `w` units in,
\* before the next frame's gate holds it, or None once nothing holds it back.
Room(s, w) ==
    LET lead == w - rd[s]
        room == IF lead <= PaceWindow THEN PaceWindow - lead
                ELSE IF held[s] = "held" THEN 0 ELSE None
    IN IF room = None THEN None ELSE Max(room - FrameLen, 0)

\* `Budgeted::shape`: let the peer send what the group has `room` for, at least the `unsealed`
\* rest of the segment, plus the slot's allowance.
Shape(r, sl, room, unsealed) ==
    LET a == Allowance(sl)
        target == IF room = None THEN SW ELSE Min(Max(room, unsealed) + a, SW)
    IN IF target < r.win \/ target - r.win >= a \div 2
       THEN Queue([r EXCEPT !.win = target])
       ELSE r

\* The pace gate, `w` units in: at a frame's start the admit gate, counting the whole next
\* frame; inside one, `Frame::poll_ready`, counting what of it is in. Open once no reader holds
\* the group (`poll_unwanted`).
Paced(s, w) ==
    LET inside == w % FrameLen
        committed == w - inside
        partial == IF inside = 0 THEN FrameLen ELSE inside
    IN held[s] = "held" /\ rd[s] < committed /\ committed + partial - rd[s] > PaceWindow

\* `Frame::unsealed` once `w` units of a frame are in: the rest of the segment being written.
Unsealed(w) ==
    LET inside == w % FrameLen IN
    IF inside = 0 /\ w > 0 THEN 0 ELSE Seg - (inside % Seg)

\* `Budgeted::pause`'s charge: the credit the stream holds once shaped to its allowance.
Charge(r) == Min(Credit(r), W)

Accept(s) ==
    /\ pc[s] = "new"
    /\ q[s].arr > 0
    /\ pc' = [pc EXCEPT ![s] = "loop"]
    /\ woken' = [woken EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<q, credited, stuckWaiting, charge, slot, wr, rd, held, refused>>

\* The stream ends: a reader that read it whole stops it, as does one evicted. The session drops
\* its stream, returning any paused credit and its slot.
Stop(s) ==
    LET r == q[s]
        cr == credited + Held(r) + (IF r.rdr = "reading" THEN Unread(r) ELSE 0)
        q1 == [q EXCEPT ![s] = [Fresh EXCEPT !.rdr = "stopped", !.arr = r.arr, !.msd = r.msd]]
    IN /\ \E p \in Pass(q1, cr) : Settle(p, [woken EXCEPT ![s] = FALSE])
       /\ pc' = [pc EXCEPT ![s] = "done"]
       /\ charge' = [charge EXCEPT ![s] = None]
       /\ slot' = [slot EXCEPT ![s] = None]
       /\ wr' = [wr EXCEPT ![s] = 0]
       /\ rd' = [rd EXCEPT ![s] = 0]
       /\ held' = [held EXCEPT ![s] = "gone"]
       /\ UNCHANGED refused

\* `Stream::poll_lease` for one unit, from `r`: hand out what arrived, or wait for it.
Lease(s, r, sl, ch) ==
    LET filled == [r EXCEPT !.filled = r.arr, !.unl = FALSE, !.leased = TRUE, !.want = FALSE,
                            !.ready = FALSE]
        f == Queue([filled EXCEPT !.ranked = filled.ranked \/ Held(filled) > 0])
        len == f.filled - f.taken
        frameRem == FrameLen - (f.taken % FrameLen)
        segRem == Seg - (f.taken % Seg)
    IN /\ slot' = [slot EXCEPT ![s] = sl]
       /\ charge' = [charge EXCEPT ![s] = ch]
       /\ IF len > 0
          THEN \E chunk \in 1..len :
                 LET take == IF chunk >= frameRem THEN frameRem ELSE Min(chunk, segRem) IN
                 /\ q' = [q EXCEPT ![s] = [f EXCEPT !.out = TRUE, !.hand = take, !.unl = TRUE]]
                 /\ pc' = [pc EXCEPT ![s] = "out"]
                 /\ UNCHANGED <<credited, stuckWaiting, woken>>
          ELSE LET waiting == Queue([f EXCEPT !.want = TRUE, !.unl = TRUE]) IN
               \E p \in Pass([q EXCEPT ![s] = waiting], credited) :
                 \* Parked, unless the pass evicted the stream itself.
                 /\ Settle(p, [woken EXCEPT ![s] = p.q[s].rdr = "evicted"])
                 /\ pc' = [pc EXCEPT ![s] = "loop"]

\* Park on the pace gate and the stream's reset (`poll_pace`).
Park(s) ==
    /\ pc' = [pc EXCEPT ![s] = "paused"]
    /\ woken' = [woken EXCEPT ![s] = FALSE]

\* One turn of `Reader::poll_read_frame`'s loop, or of the frame admission before it. The gate
\* is checked only at a seal, so a paused stream holds no segment's credit.
Step(s) ==
    LET r == q[s]
        sealed == r.rel = r.taken
        sl == SlotOf(s)
    IN
    /\ pc[s] \in {"loop", "paused"}
    /\ woken[s]
    /\ IF r.rdr = "evicted" \/ r.taken = Len
       THEN Stop(s)
       ELSE IF sealed /\ Paced(s, wr[s])
       THEN \* `Budgeted::pause`: reserve the credit the stream holds, or read on.
            IF charge[s] # None
            THEN Park(s) /\ UNCHANGED <<q, credited, stuckWaiting, charge, slot, wr, rd, held,
                                        refused>>
            ELSE LET shrunk == Queue([r EXCEPT !.win = Allowance(sl)])
                     ch == Charge(shrunk)
                 IN IF PausedTotal(charge) + ch <= Cap
                    THEN /\ Park(s)
                         /\ q' = [q EXCEPT ![s] = shrunk]
                         /\ charge' = [charge EXCEPT ![s] = ch]
                         /\ slot' = [slot EXCEPT ![s] = sl]
                         /\ UNCHANGED <<credited, stuckWaiting, wr, rd, held, refused>>
                    ELSE /\ Lease(s, shrunk, sl, None)
                         /\ refused' = TRUE
                         /\ UNCHANGED <<wr, rd, held>>
       ELSE IF sealed
       THEN \* The gate is open: resume, and shape the window, at a frame's start for the
            \* admission and then for the frame, whose first segment is whole.
            LET room == Room(s, wr[s])
                admitted == IF wr[s] % FrameLen = 0 THEN Shape(r, sl, room, 0) ELSE r
            IN /\ Lease(s, Shape(admitted, sl, room, Seg), sl, None)
               /\ UNCHANGED <<wr, rd, held, refused>>
       ELSE Lease(s, r, slot[s], charge[s]) /\ UNCHANGED <<wr, rd, held, refused>>

\* Hand the lease back (`Lease::release` at a seal, a drop short of one, which restores it). The
\* lease carries the window shaped to the room the chunk leaves (`Lease::set_window`), which the
\* hand back sets under the same lock, before it queues the grant.
HandBack(s) ==
    LET r == q[s]
        tk == r.taken + r.hand
        sl == SlotOf(s)
        shaped == Shape(r, sl, Room(s, tk), Unsealed(tk))
        back == Queue([shaped EXCEPT !.out = FALSE, !.taken = tk, !.hand = 0])
        released == [back EXCEPT !.rel = tk]
        passes == IF tk % Seg = 0 THEN Pass([q EXCEPT ![s] = released], credited + tk - r.rel)
                  ELSE IF stuckWaiting THEN Pass([q EXCEPT ![s] = back], credited)
                  ELSE Unpassed([q EXCEPT ![s] = back], credited)
    IN /\ pc[s] = "out"
       /\ \E p \in passes : Settle(p, woken)
       /\ pc' = [pc EXCEPT ![s] = "write"]
       /\ slot' = [slot EXCEPT ![s] = sl]
       /\ UNCHANGED <<charge, wr, rd, held, refused>>

\* Write the chunk into the frame, committing it once whole. The window was shaped to the room
\* it leaves as the lease went back.
Write(s) ==
    LET w == q[s].taken
    IN /\ pc[s] = "write"
       /\ wr' = [wr EXCEPT ![s] = w]
       /\ pc' = [pc EXCEPT ![s] = "loop"]
       /\ UNCHANGED <<q, credited, stuckWaiting, woken, charge, slot, rd, held, refused>>

(***************************************************************************)
(* The driver, the peer, and the group's reader.                           *)
(***************************************************************************)
\* The driver builds a packet carrying the queued `MAX_STREAM_DATA`, computing its limit now.
Transmit(s) ==
    /\ q[s].due
    /\ q' = [q EXCEPT ![s].due = FALSE,
                      ![s].msd = IF CanGrant(q[s]) THEN Limit(q[s]) ELSE q[s].msd]
    /\ UNCHANGED <<credited, stuckWaiting, pc, woken, charge, slot, wr, rd, held, refused>>

\* The peer sends one unit, which arrives: `StreamsState::received`.
Send(s) ==
    LET r == q[s]
        reading == r.rdr = "reading"
        arrived == IF reading
                   THEN [r EXCEPT !.arr = r.arr + 1, !.ready = r.ready \/ r.want, !.want = FALSE,
                                  !.ranked = r.ranked \/ r.leased, !.unl = TRUE]
                   ELSE [r EXCEPT !.arr = r.arr + 1]
        cr == IF reading THEN credited ELSE credited + 1
    IN /\ r.rdr # "stopped"
       /\ r.arr < Len
       /\ r.arr < r.msd
       /\ DataRecvd(q) < W + credited
       /\ \E p \in Pass([q EXCEPT ![s] = arrived], cr) :
            Settle(p, IF reading THEN Wake(woken, {s}) ELSE woken)
       /\ UNCHANGED <<pc, charge, slot, wr, rd, held, refused>>

\* The group's reader takes the next committed frame, waking a paused writer.
Take(s) ==
    /\ pc[s] # "done"
    /\ held[s] = "held"
    /\ rd[s] + FrameLen <= wr[s] - (wr[s] % FrameLen)
    /\ rd' = [rd EXCEPT ![s] = rd[s] + FrameLen]
    /\ woken' = [woken EXCEPT ![s] = woken[s] \/ pc[s] = "paused"]
    /\ UNCHANGED <<q, credited, stuckWaiting, pc, charge, slot, wr, held, refused>>

\* The group's first reader arrives, starting where readers got even.
Arrive(s) ==
    /\ pc[s] # "done"
    /\ held[s] = "late"
    /\ held' = [held EXCEPT ![s] = "held"]
    /\ UNCHANGED <<q, credited, stuckWaiting, pc, woken, charge, slot, wr, rd, refused>>

\* The group's last reader leaves, which the writer's pace gate watches for.
Drop(s) ==
    /\ Leave
    /\ pc[s] # "done"
    /\ held[s] = "held"
    /\ held' = [held EXCEPT ![s] = "gone"]
    /\ woken' = [woken EXCEPT ![s] = woken[s] \/ pc[s] = "paused"]
    /\ UNCHANGED <<q, credited, stuckWaiting, pc, charge, slot, wr, rd, refused>>

Done == \A s \in Streams : pc[s] = "done"

Init ==
    /\ q = [s \in Streams |-> Fresh]
    /\ credited = 0
    /\ stuckWaiting = FALSE
    /\ pc = [s \in Streams |-> "new"]
    /\ woken = [s \in Streams |-> FALSE]
    /\ charge = [s \in Streams |-> None]
    /\ slot = [s \in Streams |-> None]
    /\ wr = [s \in Streams |-> 0]
    /\ rd = [s \in Streams |-> 0]
    /\ held \in [Streams -> IF Late THEN {"late", "held"} ELSE {"held"}]
    /\ refused = FALSE

\* The steps weak fairness covers: all but a reader arriving or leaving, which may never happen.
SystemOf(s) == Send(s) \/ Transmit(s) \/ Accept(s) \/ Step(s) \/ HandBack(s) \/ Write(s) \/ Take(s)
System == \E s \in Streams : SystemOf(s)

Next ==
    \/ System
    \/ \E s \in Streams : Arrive(s) \/ Drop(s)
    \/ Done /\ UNCHANGED vars

Spec == Init /\ [][Next]_vars /\ \A s \in Streams : WF_vars(SystemOf(s))

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)
\* The books balance: every unit arrived is held by one stream or was credited back.
Books == DataRecvd(q) = credited + SumTo(LAMBDA s: Hold(q[s]), N)

\* What every stream holds together (unread units, leased units not released, the frame partly
\* in) stays within the connection window.
WithinWindow == SumTo(LAMBDA s: Hold(q[s]), N) <= W

\* Paused streams hold at most what they reserved, and that at most half the window.
PausedWithinCap ==
    /\ PausedTotal(charge) <= Cap
    /\ \A s \in Streams : charge[s] # None => Credit(q[s]) <= charge[s]

\* Every stream pauses, charged its slot's allowance.
EveryPauseFits == ~refused
ChargedAllowance == \A s \in Streams : charge[s] # None => charge[s] <= Allowance(slot[s])

\* Every stream is eventually read whole or evicted, and stopped.
AllEnd == \A s \in Streams : <>(pc[s] = "done")

(***************************************************************************)
(* `AllEnd` by two safety checks, which TLC runs as fast as the rest: its  *)
(* liveness search evaluates every step's enabling at every state, a       *)
(* thousand times slower here.                                             *)
(*                                                                         *)
(* `Progress`: every step other than the final stutter raises a bounded    *)
(* measure: first the units arrived, filled, taken (counted twice, so a    *)
(* chunk handed back outweighs the hand it leaves), in hand, released, and *)
(* written, the frames read, the grants sent, and each stream's and        *)
(* reader's lifecycle; then, with those equal, the tasks parked; then the  *)
(* grants not queued. So every behavior ends stuttering in one state, and  *)
(* weak fairness leaves it only where no step of the system is enabled.    *)
(* `Ends`: every such state has every stream done.                         *)
(***************************************************************************)
Lifecycle(s) ==
    (CASE pc[s] = "new" -> 0 [] pc[s] = "done" -> 2 [] OTHER -> 1)
    \* An eviction discards the leased units, so it outweighs them.
    + (CASE q[s].rdr = "reading" -> 0 [] q[s].rdr = "evicted" -> Len + 1 [] OTHER -> 2 * Len + 2)
    + (CASE held[s] = "late" -> 0 [] held[s] = "held" -> 1 [] OTHER -> 2)

\* A done stream counts above anything a live one reaches, since `Stop` clears its books.
Counted == SumTo(LAMBDA s: IF pc[s] = "done" THEN 12 * Len + SW + 6
                           ELSE q[s].arr + q[s].filled + 2 * q[s].taken + q[s].hand + q[s].rel
                                + wr[s] + q[s].msd + rd[s] + Lifecycle(s), N)
Parked == Cardinality({s \in Streams : ~woken[s]})
Undue == Cardinality({s \in Streams : ~q[s].due})

Progress ==
    [][\/ Done
       \/ Counted' > Counted
       \/ Counted' = Counted /\ Parked' > Parked
       \/ Counted' = Counted /\ Parked' = Parked /\ Undue' > Undue]_vars

\* Where some step of the system is enabled: each step's guard, since the rest of every step
\* always has a successor (a pass always has an outcome, and a lease a chunk to take).
\* `Guarded` checks the two agree.
Enabled(s) ==
    \/ q[s].rdr # "stopped" /\ q[s].arr < Len /\ q[s].arr < q[s].msd /\ DataRecvd(q) < W + credited
    \/ q[s].due
    \/ pc[s] = "new" /\ q[s].arr > 0
    \/ pc[s] \in {"loop", "paused"} /\ woken[s]
    \/ pc[s] \in {"out", "write"}
    \/ pc[s] # "done" /\ held[s] = "held" /\ rd[s] + FrameLen <= wr[s] - (wr[s] % FrameLen)

Guarded == (\E s \in Streams : Enabled(s)) = ENABLED System

Ends == Done \/ \E s \in Streams : Enabled(s)

\* Witnesses, each expected to fail: TLC reaching them shows the model is not vacuous.
NeverPaused == \A s \in Streams : pc[s] # "paused"
NeverEvicted == \A s \in Streams : q[s].rdr # "evicted"
NeverStuckWaiting == ~stuckWaiting
NeverPartial == \A s \in Streams : q[s].taken = q[s].rel
NeverPausedInside == \A s \in Streams : pc[s] = "paused" => wr[s] % FrameLen = 0
=============================================================================
