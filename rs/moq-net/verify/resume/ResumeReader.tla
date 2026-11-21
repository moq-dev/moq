---------------------------- MODULE ResumeReader ----------------------------
(***************************************************************************)
(* One reader of a logical track that a front serves across route changes, *)
(* as `model::resume` (rs/moq-net/src/model/resume.rs) runs it.            *)
(*                                                                         *)
(* Each route's copy is a received track whose groups land in any order.   *)
(* The front (`resume::Producer`) serves one copy at a time, switches to   *)
(* another live one at will, and ends the track. The reader (`Reader`)     *)
(* keeps one `Copy` per route it read, each with its own cursor over that  *)
(* track: a walk in arrival order, the groups a widening owes it, and what *)
(* it took. One poll for a group (`Reader::poll_lowest`) is one atomic     *)
(* step, as the reader's lock makes it; so is a switch, which syncs the    *)
(* reader as `serve_from` does.                                            *)
(*                                                                         *)
(* Scope: whole groups (no frames, so no `Recover`), an unbounded          *)
(* subscription from the oldest group with an hour's budget (nothing goes  *)
(* stale or expires), arrival order, no local range, copies that answer at *)
(* once, and a reader that joins before any group lands. A route declares  *)
(* its end once it holds the last group, a gap below it may still land,    *)
(* even once its session settled that end, and a route that declared its   *)
(* end never fails.                                                        *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    G,            \* groups 0 .. G - 1; a finished route declares its end at G
    MaxTracks,    \* routes that come up, each a fresh track (`Op::Up`)
    MaxSwitches,  \* front switches to a live route other than a failed one
    Hold          \* whether the reader holds a group while it reads it, keeping its copy

Groups == 0 .. G - 1
Tracks == 1 .. MaxTracks

\* `Option<u64>` as an integer: None sorts below every group, as in Rust.
None == -1
\* `Copy::until` is `Option<Option<u64>>`: NoBound for `None`, None for
\* `Some(None)` (bounded before any group), else the last group taken.
NoBound == -2
\* Past every group: the end of an unbounded window.
Inf == G + 1

Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
Max2(a, b) == IF a >= b THEN a ELSE b

VARIABLES
    trk,       \* trk[t]: route t's track: made, failed, fin, and arr (arrival order)
    front,     \* the front: cur (serving track), gen, end, switches
    rd,        \* the reader (`Reader`), see Init
    reading    \* groups handed out and still read, each holding its copy's lease

vars == <<trk, front, rd, reading>>

Landed(t) == {trk[t].arr[i] : i \in 1 .. Len(trk[t].arr)}
Live(t) == trk[t].made /\ ~trk[t].failed
\* `track::Consumer::latest_key` (`TrackState::max_key`): the newest group the track holds.
Latest(t) == IF Landed(t) = {} THEN None ELSE Max(Landed(t))

\* `TrackState::is_complete`: the end is declared and the tail holds every group
\* from where the feed starts (its first arrival) up to it. Sealing records what
\* the track held then: a group landing below the end afterwards is not owed.
Seal(x) ==
    LET got == {x.arr[i] : i \in 1 .. Len(x.arr)} IN
    IF /\ ~x.whole /\ x.fin
       /\ \A g \in Min(got) .. G - 1 : g \in got
        THEN [x EXCEPT !.whole = TRUE, !.held = got]
        ELSE x
\* What a track owes its readers: what it held once complete, else all it holds.
Held(t) == IF trk[t].whole THEN trk[t].held ELSE Landed(t)

(***************************************************************************)
(* A copy and its cursor (`Copy` plus its `track::Subscriber`).            *)
(***************************************************************************)

\* `Copy::start`: where its local range starts.
Start(c) == IF c.floor = None THEN 0 ELSE c.floor
Bounded(c) == c.until # NoBound
\* `Copy::end`: the first group past where it was replaced.
End(c) == IF Bounded(c) THEN c.until + 1 ELSE Inf
Admits(floor, until, g) ==
    /\ g >= (IF floor = None THEN 0 ELSE floor)
    /\ (until = NoBound \/ g <= until)

NewCopy(id, t, gen, floor, rep, until) ==
    [id |-> id, t |-> t, gen |-> gen, floor |-> floor, rep |-> rep,
     until |-> until, done |-> "no", drained |-> 0, idx |-> 1,
     took |-> {}, owed |-> {}]

\* The wire asks for [floor, until] instead (`Copy::cap`, or `Copy::send` on a
\* reuse or a widening; neither reopens a drained copy): the cursor
\* passes what its window leaves out, holding nothing, and a widening owes it
\* the cached groups the window newly covers.
Window(c, floor, until) ==
    [c EXCEPT
        !.floor = floor,
        !.until = until,
        !.owed = {g \in @ : Admits(floor, until, g)}
                 \cup {g \in Landed(c.t) :
                         /\ Admits(floor, until, g)
                         /\ ~Admits(c.floor, c.until, g)
                         /\ g \notin c.took}]

\* `Reader::deliverable`.
Deliverable(c, g, dlv) ==
    /\ ~(Bounded(c) /\ g > c.until)
    /\ g \notin dlv

\* One `poll_peek_group` on a copy's cursor, skipping what the logical reader
\* already has (`take`, so the cursor moves past it). Returns the cursor moved,
\* and the group it reserves (None when pending or done), and where it came from.
RECURSIVE Peek(_, _, _)
Peek(c, dlv, fuel) ==
    LET ow == {g \in c.owed : Admits(c.floor, c.until, g) /\ g \notin c.took}
        arr == trk[c.t].arr
    IN
    IF fuel = 0 THEN [c |-> c, g |-> None, src |-> "none"]
    \* What a widening owes comes first, newest first (`Order::Descending`). An
    \* aborted track still hands out what it holds before its error.
    ELSE IF ow # {} THEN
        LET g == Max(ow) IN
        IF Deliverable(c, g, dlv)
            THEN [c |-> c, g |-> g, src |-> "owed"]
            ELSE Peek([c EXCEPT !.took = @ \cup {g}, !.owed = @ \ {g}], dlv, fuel - 1)
    \* `TrackState::poll_recv_group`: past the last arrival, a complete track
    \* ends the walk, and an aborted one fails it. A clean end records the
    \* arrivals it saw (`Copy::drained`).
    ELSE IF c.idx > Len(arr) THEN
        IF trk[c.t].whole
            THEN [c |-> [c EXCEPT !.done = "ok", !.drained = Len(arr)], g |-> None, src |-> "none"]
        ELSE IF trk[c.t].failed
            THEN [c |-> [c EXCEPT !.done = "err"], g |-> None, src |-> "none"]
            ELSE [c |-> c, g |-> None, src |-> "none"]
    ELSE
        LET g == arr[c.idx] IN
        IF ~Admits(c.floor, c.until, g) \/ g \in c.took
            THEN Peek([c EXCEPT !.idx = @ + 1], dlv, fuel - 1)
        ELSE IF Deliverable(c, g, dlv)
            THEN [c |-> c, g |-> g, src |-> "walk"]
        ELSE Peek([c EXCEPT !.took = @ \cup {g}, !.idx = @ + 1], dlv, fuel - 1)

Fuel == 2 * G + 2

Covers(dlv, lo, hi) == \A g \in lo .. hi - 1 : g \in dlv

\* `TrackState::may_land`: a group in [lo, hi) the reader lacks may still land
\* on the track, below its declared end. Without a floor, only from its lowest
\* arrival on. The hour's budget is also the grace a gap is waited for.
MayLand(t, lo, hi, floored, dlv) ==
    /\ Live(t)
    /\ \E g \in lo .. (IF hi > G THEN G ELSE hi) - 1 :
        /\ g \notin dlv
        /\ g \notin Landed(t)
        /\ floored \/ (Landed(t) # {} /\ g >= Min(Landed(t)))

\* `track::Subscriber::has_unread_group`: a group the walk has yet to take, or
\* a widening owes, in [Start, end), that the reader lacks.
HasUnread(c, end, dlv) ==
    LET arr == trk[c.t].arr
        ahead == {arr[i] : i \in c.idx .. Len(arr)} \cup c.owed
    IN /\ ~trk[c.t].failed
       /\ \E g \in ahead : g >= Start(c) /\ g < end /\ g < End(c) /\ g \notin dlv

(***************************************************************************)
(* The reader.                                                             *)
(***************************************************************************)

Serving(r) ==
    IF Len(r.copies) > 0 /\ r.copies[Len(r.copies)].gen = r.gen
        THEN {r.copies[Len(r.copies)]}
        ELSE {}

\* `Reader::boundary`: where a switch splits the subscription, the largest
\* group any copy holds, never below the newest handed out.
Boundary(r) ==
    Max2(r.newest, Max({None} \cup {Latest(r.copies[i].t) : i \in 1 .. Len(r.copies)}))

\* `Copy::cap`: ask for nothing past `until`; a cap only narrows.
Cap(c, until) ==
    IF Bounded(c) /\ c.until <= until THEN c ELSE Window(c, c.floor, until)

\* `Reader::sync`: follow the front to its serving copy. The switch splits the
\* subscription at the boundary: the new copy starts there, and every copy it
\* replaces is capped there at once, or dropped while there is none.
Sync(r, f) ==
    IF r.gen = f.gen THEN r
    ELSE IF f.cur = 0 THEN [r EXCEPT !.gen = f.gen]
    ELSE IF Len(r.copies) > 0 /\ r.copies[Len(r.copies)].gen = f.gen
        THEN [r EXCEPT !.gen = f.gen]
    ELSE
        LET bnd == Boundary(r)
            same == {i \in 1 .. Len(r.copies) : r.copies[i].t = f.cur}
            \* The returning copy is left out of the caps.
            back == IF same # {} THEN Max(same) ELSE 0
            capped == [i \in 1 .. Len(r.copies) |->
                        IF i = back THEN r.copies[i]
                        ELSE [Cap(r.copies[i], bnd) EXCEPT !.rep = TRUE]]
            \* With no boundary nothing is held nor handed out: nothing to split,
            \* so the copies it replaces are dropped.
            reps == IF bnd # None THEN capped
                    ELSE IF back # 0 THEN <<r.copies[back]>> ELSE <<>>
            at == IF bnd # None THEN back ELSE IF back # 0 THEN 1 ELSE 0
        IN
        IF at # 0 THEN
            \* Returning to a route reuses its copy, from the boundary.
            LET c == reps[at]
                c2 == Window([c EXCEPT !.gen = f.gen, !.rep = FALSE, !.done = "no"],
                             Max2(c.floor, bnd), NoBound)
                rest == [j \in 1 .. Len(reps) - 1 |-> IF j < at THEN reps[j] ELSE reps[j + 1]]
            IN [r EXCEPT !.gen = f.gen, !.resumed = TRUE, !.copies = rest \o <<c2>>]
        ELSE
            [r EXCEPT !.gen = f.gen, !.resumed = TRUE, !.next = @ + 1,
                      !.copies = reps \o <<NewCopy(r.next, f.cur, f.gen, bnd, FALSE, NoBound)>>]

\* `Reader::backfill`: ask the newest copy's route for [lo, hi), widening an
\* earlier bounded copy of that route that asks from no later than lo, and
\* reading it again if it ran out.
Backfill(r, lo, hi) ==
    LET n == Len(r.copies)
        newest == r.copies[n]
        asks(c) == c.t = newest.t /\ c.floor # None /\ c.floor <= lo
    IN
    IF Covers(r.dlv, lo, hi) THEN r
    ELSE IF \E i \in 1 .. n : asks(r.copies[i]) /\ End(r.copies[i]) >= hi THEN r
    ELSE
        LET widen == {i \in 1 .. n - 1 :
                        /\ r.copies[i].done # "err"
                        /\ Bounded(r.copies[i])
                        /\ asks(r.copies[i])}
        IN
        IF widen # {} THEN
            LET i == Min(widen)
                c == r.copies[i]
            IN [r EXCEPT !.copies[i] = [Window(c, c.floor, hi - 1) EXCEPT !.done = "no"]]
        ELSE
            [r EXCEPT !.next = @ + 1,
                      !.copies = SubSeq(@, 1, n - 1)
                                 \o <<NewCopy(r.next, newest.t, newest.gen, lo, TRUE, hi - 1)>>
                                 \o <<newest>>]

\* `Reader::backfill_gaps`: after a switch, ask the serving route for what the
\* reader lacks below its copy's resume point, from the lowest gap no replaced
\* copy still drains: a live one holding it under its cap.
BackfillGaps(r) ==
    IF ~r.resumed THEN r
    ELSE
        LET r1 == [r EXCEPT !.resumed = FALSE]
            n == Len(r.copies)
        IN
        IF n = 0 \/ r.copies[n].floor = None THEN r1
        ELSE
            LET at == r.copies[n].floor
                drains(g) ==
                    \E i \in 1 .. n :
                        LET c == r.copies[i] IN
                        /\ c.rep /\ c.done = "no"
                        /\ Start(c) <= g /\ g < End(c)
                        /\ ~trk[c.t].failed
                        /\ g \in Landed(c.t)
                gaps == {g \in 0 .. at - 1 : g \notin r.dlv /\ ~drains(g)}
            IN IF gaps = {} THEN r1 ELSE Backfill(r1, Min(gaps), at)

\* The copy loop of `poll_lowest`: peek each copy, keeping the lowest group;
\* a tie goes to the newer copy. A copy that ran out cleanly is read again
\* once a group arrived on it since (`Copy::drained`); one arriving on the
\* serving copy, which resumed at a floor, may be under it (`late`), as may
\* what a replaced copy that just failed still had to drain.
RECURSIVE Scan(_, _, _, _, _, _)
Scan(cs, dlv, gen, i, best, late) ==
    IF i > Len(cs) THEN [cs |-> cs, best |-> best, late |-> late]
    ELSE
        LET c0 == cs[i]
            spent == Bounded(c0) /\ Covers(dlv, 0, End(c0))
            reopens == c0.done = "ok" /\ ~spent /\ Len(trk[c0.t].arr) > c0.drained
            c == IF reopens THEN [c0 EXCEPT !.done = "no"] ELSE c0
            serves == ~c.rep /\ c.gen = gen /\ c.floor # None
        IN
        IF c.done # "no" \/ spent
            THEN Scan(cs, dlv, gen, i + 1, best, late)
        ELSE
            LET p == Peek(c, dlv, Fuel)
                cs2 == [cs EXCEPT ![i] = p.c]
                better == p.g # None /\ (best.g = None \/ p.g <= best.g)
            IN Scan(cs2, dlv, gen, i + 1,
                    IF better THEN [i |-> i, g |-> p.g, src |-> p.src] ELSE best,
                    late \/ (reopens /\ serves) \/ (c.rep /\ p.c.done = "err"))

NoBest == [i |-> 0, g |-> None, src |-> "none"]

\* `Reader::take`: hand out the group. The switch capped every copy it
\* replaced already, so the group bounds nothing.
Take(r, b) ==
    LET c == r.copies[b.i]
        moved == IF b.src = "walk"
                    THEN [c EXCEPT !.took = @ \cup {b.g}, !.idx = @ + 1]
                    ELSE [c EXCEPT !.took = @ \cup {b.g}, !.owed = @ \ {b.g}]
    IN [r EXCEPT !.copies[b.i] = moved,
                 !.newest = Max2(@, b.g),
                 !.dup = @ \/ b.g \in r.dlv,
                 !.dlv = @ \cup {b.g}]

\* `Copy::retains` for a replaced copy: "wait" while it may still deliver a
\* group the reader lacks under its cap, "keep" while it backs a group being
\* read, else "no". The hour's budget never passes, so neither `since` (from
\* the serving copy's edge or `Reader::drained`) nor `expired` is modelled.
Retains(c, dlv) ==
    LET backs(g) == g \in Landed(c.t)
                    \/ MayLand(c.t, g, g + 1, c.floor # None /\ c.floor <= g, {})
    IN
    IF /\ c.done = "no" /\ Bounded(c)
       /\ \/ MayLand(c.t, Start(c), End(c), c.floor # None, dlv)
          \/ HasUnread(c, End(c), dlv)
        THEN "wait"
    ELSE IF \E x \in reading : x.cid = c.id \/ backs(x.g)
        THEN "keep"
    ELSE "no"

\* `Reader::let_go`: drop the replaced copies nothing needs; whether any waits.
LetGo(r) ==
    LET n == Len(r.copies)
        ret == [i \in 1 .. n |->
                    IF r.copies[i].rep THEN Retains(r.copies[i], r.dlv) ELSE "serving"]
        kept == {i \in 1 .. n : ret[i] # "no"}
        order == CHOOSE s \in [1 .. Cardinality(kept) -> kept] :
                    \A a, b \in 1 .. Cardinality(kept) : a < b => s[a] < s[b]
    IN [copies |-> [k \in 1 .. Cardinality(kept) |-> r.copies[order[k]]],
        waits |-> \E i \in 1 .. n : ret[i] = "wait"]

\* `Reader::poll_end`, with `poll_lowest`'s guard: a drained serving copy ends
\* the track only once no copy it replaced may still deliver below the end.
PollEnd(r, drained, waits) ==
    LET s == Serving(r)
        ok == IF drained /\ waits THEN "no" ELSE "ok"
    IN
    IF \E c \in s : c.done = "ok" THEN ok
    ELSE IF front.end = "err" THEN "err"
    ELSE IF front.end = "ok" /\ (s = {} \/ \E c \in s : c.done # "no") THEN ok
    ELSE "no"

\* One `poll_recv_group` on the logical track (`Reader::poll_lowest`, looked
\* at again while it returns `None`: a late group on the serving copy sets
\* `resumed`, so the next pass asks its route for what lies under its floor).
RECURSIVE Pass(_)
Pass(r1) ==
    LET sc == Scan(r1.copies, r1.dlv, r1.gen, 1, NoBest, FALSE)
        r2 == [r1 EXCEPT !.copies = sc.cs, !.resumed = @ \/ sc.late]
    IN
    IF sc.best.g # None THEN [r |-> Take(r2, sc.best), cid |-> r2.copies[sc.best.i].id, g |-> sc.best.g]
    ELSE IF sc.late THEN Pass(BackfillGaps(r2))
    ELSE
        LET drained == \E c \in Serving(r2) : c.done = "ok"
            lg == LetGo(r2)
            r4 == [r2 EXCEPT !.copies = lg.copies]
        IN [r |-> [r4 EXCEPT !.ended = PollEnd(r4, drained, lg.waits)], cid |-> 0, g |-> None]

Poll(r) == Pass(BackfillGaps(Sync(r, front)))

\* What a clean end owes the reader: every group its serving route held once
\* complete, and, as `resume_model`'s oracle owes, every group all live routes
\* hold.
Owed(r) ==
    {g \in Groups :
        \/ Len(r.copies) > 0 /\ g \in Held(r.copies[Len(r.copies)].t)
        \/ \A t \in Tracks : Live(t) => g \in Held(t)}

\* What `resume_model`'s oracle owes a read that ends cleanly: every group
\* all live routes hold as it ends, late ones included. One landing after it
\* is not owed.
Carried ==
    {g \in Groups : \A t \in Tracks : Live(t) => g \in Landed(t)}

\* The groups the reader lacks that a replaced copy it still holds has unread,
\* or may still get by the rule `may_land` applies, within what it asks for.
Late(r) ==
    {g \in Groups \ r.dlv :
        \E i \in 1 .. Len(r.copies) :
            LET c == r.copies[i] IN
            /\ c.rep /\ c.done = "no"
            /\ Admits(c.floor, c.until, g)
            /\ \/ /\ ~trk[c.t].failed
                  /\ g \in {trk[c.t].arr[k] : k \in c.idx .. Len(trk[c.t].arr)} \cup c.owed
               \/ MayLand(c.t, g, g + 1, c.floor # None, r.dlv)}

(***************************************************************************)
(* Steps.                                                                  *)
(***************************************************************************)

Init ==
    /\ trk = [t \in Tracks |-> [made |-> t = 1, failed |-> FALSE, fin |-> FALSE, arr |-> <<>>,
                                whole |-> FALSE, held |-> {}]]
    /\ front = [cur |-> 1, gen |-> 1, end |-> "open", switches |-> 0]
    \* Subscribed to the serving copy at once (`Consumer::subscribe`).
    /\ rd = [gen |-> 1, next |-> 2, newest |-> None, dlv |-> {}, resumed |-> TRUE,
             ended |-> "no", dup |-> FALSE, late |-> {}, carried |-> {},
             copies |-> <<NewCopy(1, 1, 1, None, FALSE, NoBound)>>]
    /\ reading = {}

\* A route comes up with a fresh track (`Op::Up`).
Up(t) ==
    /\ ~trk[t].made
    /\ \A u \in Tracks : u < t => trk[u].made
    /\ trk' = [trk EXCEPT ![t].made = TRUE]
    /\ UNCHANGED <<front, rd, reading>>

\* A route's session fails, aborting its track (`Op::Down`).
Down(t) ==
    /\ Live(t) /\ ~trk[t].fin
    /\ trk' = [trk EXCEPT ![t].failed = TRUE]
    /\ UNCHANGED <<front, rd, reading>>

\* A group lands on a route's track (`Op::Write`), even past its declared end.
Land(t, g) ==
    /\ Live(t)
    /\ g \notin Landed(t)
    /\ trk' = [trk EXCEPT ![t] = Seal([@ EXCEPT !.arr = Append(@, g)])]
    /\ UNCHANGED <<front, rd, reading>>

\* A route declares the end at G once it holds the last group (`Op::End`).
Finish(t) ==
    /\ Live(t) /\ ~trk[t].fin
    /\ G - 1 \in Landed(t)
    /\ trk' = [trk EXCEPT ![t] = Seal([@ EXCEPT !.fin = TRUE])]
    /\ UNCHANGED <<front, rd, reading>>

\* The route's session settles its declared end (`Producer::finish_shared` on a
\* received copy): it has every group stream it will get, so the track is
\* complete with what it holds, gaps and all. A group still in flight below
\* the end may land afterwards, and is not owed by the route.
Settle(t) ==
    /\ Live(t) /\ trk[t].fin /\ ~trk[t].whole
    /\ trk' = [trk EXCEPT ![t].whole = TRUE, ![t].held = Landed(t)]
    /\ UNCHANGED <<front, rd, reading>>

\* The front serves another live route (`Producer::splice`), and every reader
\* follows at once (`serve_from`). Leaving a failed route is not counted. A
\* copy the front serves after a clean end (`Action::Reroute`) is for group
\* reads and fetches, not this reader: `Reader::sync` stays on the generation
\* and copy the track ended on (`Phase::Finished`), so it is no step here.
Serve(t) ==
    /\ front.end = "open"
    /\ Live(t) /\ t # front.cur
    /\ front.switches < MaxSwitches \/ ~Live(front.cur)
    /\ front' = [front EXCEPT !.cur = t, !.gen = @ + 1,
                              !.switches = @ + (IF Live(front.cur) THEN 1 ELSE 0)]
    /\ rd' = IF rd.ended = "no" THEN Sync(rd, front') ELSE rd
    /\ UNCHANGED <<trk, reading>>

\* The serving route's copy completed (`Front::track_ended`, `Action::Finish`).
\* A replaced source finishing ends the track only while it drains, which stops
\* once its replacement is spliced, so after a switch only the serving one does.
FrontFinish ==
    /\ front.end = "open"
    /\ trk[front.cur].whole
    /\ front' = [front EXCEPT !.end = "ok"]
    /\ UNCHANGED <<trk, rd, reading>>

\* No route is left: the front ends the track unroutable.
FrontFail ==
    /\ front.end = "open"
    /\ \A t \in Tracks : ~Live(t)
    /\ front' = [front EXCEPT !.end = "err"]
    /\ UNCHANGED <<trk, rd, reading>>

\* The reader polls for a group, whether or not it was woken (`Op::Read`).
Read ==
    /\ rd.ended = "no"
    /\ LET p == Poll(rd)
           r == IF p.r.ended = "ok" THEN [p.r EXCEPT !.late = Late(p.r), !.carried = Carried] ELSE p.r
       IN /\ rd' = r
          /\ reading' = IF Hold /\ p.g # None THEN reading \cup {[g |-> p.g, cid |-> p.cid]} ELSE reading
    /\ rd' # rd
    /\ UNCHANGED <<trk, front>>

\* The reader finishes a group, dropping its lease.
Drop(x) ==
    /\ x \in reading
    /\ reading' = reading \ {x}
    /\ UNCHANGED <<trk, front, rd>>

\* The read is over.
Done ==
    /\ rd.ended # "no"
    /\ UNCHANGED vars

Next ==
    \/ \E t \in Tracks : Up(t) \/ Down(t) \/ Finish(t) \/ Settle(t) \/ Serve(t)
    \/ \E t \in Tracks, g \in Groups : Land(t, g)
    \/ FrontFinish \/ FrontFail
    \/ Read
    \/ \E x \in reading : Drop(x)
    \/ Done

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)

\* A group is handed out at most once, whichever copies deliver it.
OnceEach == ~rd.dup


\* A clean end comes with every group it owes (`Owed`), judged as routes go on,
\* and every group all live routes held as it ended (`Carried`).
CleanEndIsComplete == rd.ended = "ok" => (Owed(rd) \cup rd.carried) \subseteq rd.dlv

\* The serving copy draining ends the track only once no copy it replaced may
\* still deliver a group the reader lacks below that end (`Late`, as it ended).
EndWaitsForReplaced == rd.late = {}

\* A switch caps every copy it replaces at once, so the old route sends nothing
\* past the boundary: only the serving copy is unbounded.
ReplacedCapped == \A i \in 1 .. Len(rd.copies) : rd.copies[i].rep => Bounded(rd.copies[i])

\* And at the boundary the switch split at, not past it: a switch leaves every
\* copy it replaced capped at most there, judged against the reader as the
\* switch found it, since groups landing later raise the boundary.
SwitchCapsAtBoundary ==
    [][rd'.gen # rd.gen /\ Boundary(rd) # None =>
        \A i \in 1 .. Len(rd'.copies) :
            rd'.copies[i].rep => Bounded(rd'.copies[i]) /\ rd'.copies[i].until <= Boundary(rd)]_vars

\* A clean end only follows a route declaring one.
CleanEndIsDeclared == rd.ended = "ok" => \E t \in Tracks : trk[t].fin

\* The track fails only once the front gave up on it: a route failing is the
\* front's to replace.
FailsOnlyUnroutable == rd.ended = "err" => front.end = "err"

\* Once the serving route's copy is complete and the reader holds every group,
\* a poll ends the read rather than parking on another route.
QuietIsSettled ==
    (rd.ended = "no" /\ trk[front.cur].whole /\ rd.dlv = Groups) => Poll(rd).r # rd

\* Flapping between routes keeps at most a route's own copy and one backfill
\* of it still subscribed, however many times it flaps. A drained copy waits
\* only for the next parked poll to let it go. A route failing may add one
\* backfill below an earlier one: the groups its replaced copy was to drain,
\* under where that backfill starts, which a backfill only widens upward.
CopiesBounded ==
    \A t \in Tracks :
        Cardinality({i \in 1 .. Len(rd.copies) :
                        rd.copies[i].t = t /\ rd.copies[i].done = "no"})
            <= 2 + Cardinality({u \in Tracks : trk[u].failed})

TypeOK ==
    /\ rd.ended \in {"no", "ok", "err"}
    /\ rd.dlv \subseteq Groups
    /\ \A i \in 1 .. Len(rd.copies) : rd.copies[i].done \in {"no", "ok", "err"}

=============================================================================
