---------------------------- MODULE FailoverChain ----------------------------
(***************************************************************************)
(* One track served to a client through a chain of relays, as the relays   *)
(* of `rs/moq-net` compose: each relay's front (`model/front.rs`) serves   *)
(* the track from one upstream route at a time, and each downstream        *)
(* subscription reads it through a resume reader (`model/resume.rs`) whose *)
(* groups continue across switches through `Recover`.                      *)
(*                                                                         *)
(*   C <- E <- A <- O        the client reads relay E, which starts on A   *)
(*        E <- B <- O        B is E's alternate path                       *)
(*             B <- A        with Mesh, B may also route through A         *)
(*                                                                         *)
(* Each layer is one step its own model checked, so the state stays small: *)
(*  - A route change is one `Switch` (the router's tests check which next  *)
(*    hop routing picks; here any live upstream may win).                  *)
(*  - A switch splits the subscription at one boundary, the largest group  *)
(*    any copy holds or a reader got: it asks the new copy from there, or  *)
(*    from the lowest gap below it no live copy holds, and caps every copy *)
(*    it replaces there, stopping the boundary group at the frame the new  *)
(*    copy resumes it (`Reader::sync`, `backfill_gaps`, `Copy::cap`,       *)
(*    `verify-resume`). A reader hands each group out once, from any copy  *)
(*    its relay holds, and ends cleanly once its serving copy is complete  *)
(*    and nothing a replaced copy may still deliver is missing             *)
(*    (`OnceEach`, `CleanEndIsComplete`, `EndWaitsForReplaced` there).     *)
(*  - A group's delivery over a hop is the subscribe model's accounting    *)
(*    with an hour's budget (`verify-subscribe`): a group handed out       *)
(*    arrives unless its session dies, and nothing is dropped as stale.    *)
(*                                                                         *)
(* What this model adds is frames: a group is F frames, and a group whose  *)
(* copy fails or stalls mid-read continues on another copy at the same     *)
(* frame (`Recover`), from a copy its relay holds, from the serving copy's *)
(* subscription, or by fetching the rest from the serving route, which     *)
(* asks its own upstream in turn.                                          *)
(*                                                                         *)
(* Scope: the origin O never fails, writes each group whole, and E never   *)
(* dies, so the client always has its one hop; a dead relay never returns; *)
(* every hop subscribes before the first group, and a copy's range is      *)
(* resolved from its first group (SUBSCRIBE_OK), so a relay knows which    *)
(* groups its copy owes. A fetch reaches the serving route while that      *)
(* route lives, and a relay whose track ended still moves to another route *)
(* for the reads that hold it, forgetting the track only once none does.   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
    G,            \* groups 0 .. G - 1
    F,            \* frames per group
    MaxDeaths,    \* relays among A and B that may die
    MaxSwitches,  \* route changes, per relay, away from a live route
    Mesh,         \* whether B may route through A
    Leave         \* whether the client may unsubscribe

Groups == 0 .. G - 1
Relays == {"E", "A", "B"}
Mortal == {"A", "B"}
None == "-"

\* The routes each node may take, in the order the front prefers.
Up(n) ==
    CASE n = "C" -> {"E"}
      [] n = "E" -> {"A", "B"}
      [] n = "A" -> {"O"}
      [] n = "B" -> IF Mesh THEN {"O", "A"} ELSE {"O"}

\* A copy at d of the track as u serves it. Its reader lives at u.
Edges == {<<"C", "E">>} \cup UNION {{<<d, u>> : u \in Up(d)} : d \in Relays}
Downs(u) == {e \in Edges : e[2] = u}
CE == <<"C", "E">>

Min(S) == CHOOSE x \in S : \A y \in S : x <= y
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

VARIABLES
    org,     \* the origin's track: frames written per group, and whether it ended
    alive,   \* each relay "up", "shown" (dead, its route still in the table), or "down"
    deaths,  \* relays that died
    cp,      \* cp[e]: the copy at e[1] of the track e[2] serves, and e[2]'s reader for it
    front,   \* front[r]: the route r serves from, how its track ended, the route it
             \* ended on, switches made
    left     \* the client unsubscribed

vars == <<org, alive, deaths, cp, front, left>>

(***************************************************************************)
(* A copy, and the reader at its upstream that feeds it.                   *)
(*                                                                         *)
(*  st     "none" (not subscribed), "live", or "failed" (its session died, *)
(*         or its upstream's track failed). A failed copy keeps its cache. *)
(*  ask    the groups its subscription asks for.                           *)
(*  fetch  groups fetched outside the subscription (`fetch_group`).        *)
(*  fr     fr[g]: frames of g it holds; -1 before the upstream hands g out.*)
(*  bad    groups the upstream aborted mid-read.                           *)
(*  fin    the upstream's reader ended the subscription cleanly.           *)
(*  src    src[g]: the upstream's copy its reader reads g from (`Recover`).*)
(*  lent   lent[g]: frames of g a switch lent it, which it holds from the  *)
(*         moment g is handed out, asking the upstream only for the rest.  *)
(*  stop   stop[g]: frames of g its subscription asks for, F unless a      *)
(*         switch replaced it partway through g (a Position end).          *)
(***************************************************************************)

Fresh(ask) ==
    [st |-> "live", ask |-> ask, fetch |-> {}, fr |-> [g \in Groups |-> -1],
     bad |-> {}, fin |-> FALSE, src |-> [g \in Groups |-> None],
     lent |-> [g \in Groups |-> 0], stop |-> [g \in Groups |-> F]]

Gone ==
    [st |-> "none", ask |-> {}, fetch |-> {}, fr |-> [g \in Groups |-> -1],
     bad |-> {}, fin |-> FALSE, src |-> [g \in Groups |-> None],
     lent |-> [g \in Groups |-> 0], stop |-> [g \in Groups |-> F]]

Live(e) == cp[e].st = "live"
Held(e) == cp[e].st # "none"

\* A group the reader still owes its copy: asked or fetched, and not handed out.
Lacks(e, g) == Live(e) /\ g \in cp[e].ask \cup cp[e].fetch /\ cp[e].fr[g] = -1
\* A group handed out and still short of its last frame.
Short(e, g) == Live(e) /\ cp[e].fr[g] \in 0 .. F - 1 /\ g \notin cp[e].bad

\* The copies a relay's readers read: the origin's own track, or every copy the
\* relay still holds (the serving one and the replaced ones it keeps).
Srcs(u) == IF u = "O" THEN {"O"} ELSE {s \in Up(u) : Held(<<u, s>>)}
SrcFr(u, s, g) == IF u = "O" THEN org.fr[g] ELSE cp[<<u, s>>].fr[g]
\* The source will bring no more frames of g: its copy failed or gave up on g.
SrcDead(u, s, g) ==
    /\ u # "O"
    /\ \/ cp[<<u, s>>].st # "live"
       \/ g \in cp[<<u, s>>].bad

\* The groups some copy at u holds, from its first frame.
HeldAt(u) == {g \in Groups : \E s \in Srcs(u) : SrcFr(u, s, g) >= 0}

\* Where a switch at u splits reader e's subscription (`Reader::boundary`): the
\* largest group any copy at u holds, never below the newest e got; -1 while
\* neither exists.
Boundary(e) == Max({-1} \cup {g \in Groups : cp[e].fr[g] >= 0} \cup HeldAt(e[2]))

\* What a switch asks the new copy for: from the boundary (`sync`), or from the
\* lowest group below it the reader lacks that no live copy at u holds to drain
\* (`backfill_gaps`), or everything while there is no boundary.
ResumeAsk(e) ==
    LET u == e[2]
        bnd == Boundary(e)
        drains(g) == \E s \in Srcs(u) : Live(<<u, s>>) /\ SrcFr(u, s, g) >= 0
        gaps == {g \in cp[e].ask : g < bnd /\ cp[e].fr[g] = -1 /\ ~drains(g)}
        from == IF bnd < 0 THEN 0 ELSE IF gaps # {} THEN Min(gaps) ELSE bnd
    IN {g \in cp[e].ask : g >= from}

\* Where a switch caps the copies it replaces at u: every open reader's
\* boundary, as the copy's subscription folds them (`Copy::cap`).
CapAt(u) == Max({-1} \cup {Boundary(e) : e \in {x \in Downs(u) : Live(x) /\ ~cp[x].fin}})

\* Where a switch at u stops the boundary group on the copies it replaces (`Reader::sync`'s
\* stop, `Subscription::cap`): each open reader's boundary group, past the most frames of it
\* a copy at u holds; the highest, as the subscription folds them, and none unless every
\* reader stops.
StopAt(u) ==
    LET readers == {e \in Downs(u) : Live(e) /\ ~cp[e].fin}
        At(e) ==
            LET bnd == Boundary(e)
                held == IF bnd < 0 THEN 0
                        ELSE Max({SrcFr(u, s, bnd) : s \in Srcs(u)} \cup {0})
            IN IF held = 0 THEN {} ELSE {<<bnd, held>>}
        at == UNION {At(e) : e \in readers}
    IN IF readers = {} \/ \E e \in readers : At(e) = {} THEN {}
       ELSE {CHOOSE p \in at : \A q \in at : q[1] < p[1] \/ (q[1] = p[1] /\ q[2] <= p[2])}

\* A replaced copy stopped at a position: a stop only narrows (`Copy::cap`).
Stopped(c, at) ==
    IF at = {} THEN c
    ELSE LET p == CHOOSE p \in at : TRUE IN
         [c EXCEPT !.stop[p[1]] = IF @ < p[2] THEN @ ELSE p[2]]

\* A copy serving again asks every group whole.
Unstopped(c) == [c EXCEPT !.stop = [g \in Groups |-> F]]

\* Where a switch at u resumes its readers (`Producer::subscription`, `Reader::resume_at`):
\* each open reader's boundary group, past the most frames of it a copy at u holds; the
\* lowest of those, as a set of one, or none when any reader resumes nothing.
Resume(u) ==
    LET readers == {e \in Downs(u) : Live(e) /\ ~cp[e].fin}
        At(e) ==
            LET bnd == Boundary(e)
                held == IF bnd < 0 THEN 0
                        ELSE Max({SrcFr(u, s, bnd) : s \in Srcs(u)} \cup {0})
            IN IF held = 0 THEN {} ELSE {<<bnd, held>>}
        at == UNION {At(e) : e \in readers}
    IN IF readers = {} \/ \E e \in readers : At(e) = {} THEN {}
       ELSE {CHOOSE p \in at : \A q \in at : p[1] < q[1] \/ (p[1] = q[1] /\ p[2] <= q[2])}

\* A copy asked to resume a group past the frames held is lent them, unless it already
\* holds that group (`lend_head`); of two loans the longer stands.
Lend(u, c) ==
    IF Resume(u) = {} THEN c
    ELSE LET p == CHOOSE p \in Resume(u) : TRUE IN
         IF c.fr[p[1]] >= 0 THEN c
         ELSE [c EXCEPT !.lent[p[1]] = IF @ > p[2] THEN @ ELSE p[2]]

\* A downstream copy still reads the track at its upstream: its subscription is open, or
\* a group it was handed or fetched is not over (`TrackWeak::is_held`).
Reads(e) ==
    /\ Live(e)
    /\ \/ ~cp[e].fin
       \/ \E g \in Groups : Short(e, g) \/ (g \in cp[e].fetch /\ cp[e].fr[g] = -1)

\* What a copy at u asks: the union of its open readers' demand (`Producer::subscription`).
Demand(u) == UNION {ResumeAsk(e) : e \in {x \in Downs(u) : Live(x) /\ ~cp[x].fin}}
Wanted(u) == \E e \in Downs(u) : Live(e)

Serving(u) == <<u, front[u].cur>>
\* Where a fetch at u goes (`Route::answering`): once the track finished, the copy it
\* ended on until that copy fails, since a copy served after the end caches nothing.
Answering(u) ==
    IF front[u].end = "ok" /\ Live(<<u, front[u].last>>)
    THEN <<u, front[u].last>>
    ELSE Serving(u)
Routable(w) == IF w = "O" THEN TRUE ELSE alive[w] = "up" /\ front[w].end # "err"
\* A route still in the table: a dead relay's route stays until its session's close is
\* seen (a claim's debounce). Its copies have failed and nothing new is asked through
\* it, but a front holds a track rather than end it while it shows: a session fails the
\* copies it carries before it closes their sources, and that loss judges no track.
Shows(w) == Routable(w) \/ (w # "O" /\ alive[w] = "shown")

\* A complete copy (`TrackState::is_complete` once every group below the end
\* finished): its upstream ended it, and it holds every frame of every group
\* from the first it asked or fetched through the last (`reaches`), which a
\* copy capped below the last never does until it fetches the rest.
Whole(e) ==
    LET have == cp[e].ask \cup cp[e].fetch IN
    /\ cp[e].fin
    /\ have # {}
    /\ \A g \in Groups : g >= Min(have) => cp[e].fr[g] = F \/ g \in cp[e].bad

\* A held copy at u may still land g: it is live, lacks g, and either its
\* subscription asked for g and is not done or it fetched g, which lands after
\* the end too.
MayLand(u, g) ==
    \E s \in Up(u) :
        /\ Live(<<u, s>>)
        /\ cp[<<u, s>>].fr[g] = -1
        /\ \/ g \in cp[<<u, s>>].ask /\ ~cp[<<u, s>>].fin
           \/ g \in cp[<<u, s>>].fetch

\* The relay at u can still ask upstream for g: the copy its reads go to first is
\* live and never asked for g, so its end proves nothing about g (`Fetching`).
Askable(u, g) ==
    /\ u \in Relays
    /\ front[u].end # "err"
    /\ Live(Answering(u))
    /\ g \notin cp[Answering(u)].ask \cup cp[Answering(u)].fetch
    /\ cp[Answering(u)].fr[g] = -1

\* The reader at u has handed out all it can: every group its copy asks for is
\* out, or no copy at u holds it, may still land it, or could be asked for it
\* (`poll_end`). A reader that joined a copy asked from a later group waits for
\* the rest, as a copy waits for a recovery after its track finished.
Drained(e) ==
    LET u == e[2] IN
    \A g \in cp[e].ask :
        \/ cp[e].fr[g] >= 0
        \/ /\ \A s \in Srcs(u) : SrcFr(u, s, g) < 0
           /\ ~MayLand(u, g)
           /\ ~Askable(u, g)

\* Where a short group can continue at u: a copy holding it whose source is
\* still alive or holds a frame past the reader's (`Recover::held`).
Holders(u, e, g) ==
    {s \in Srcs(u) :
        /\ s # cp[e].src[g]
        /\ SrcFr(u, s, g) >= 0
        /\ ~SrcDead(u, s, g) \/ SrcFr(u, s, g) > cp[e].fr[g]}

\* The group's source has nothing more for the reader now.
Exhausted(e, g) == SrcFr(e[2], cp[e].src[g], g) <= cp[e].fr[g]

(***************************************************************************)
(* Steps.                                                                  *)
(***************************************************************************)

Init ==
    /\ org = [fr |-> [g \in Groups |-> -1], fin |-> FALSE]
    /\ alive = [r \in Relays |-> "up"]
    /\ deaths = 0
    \* The client subscribed through E, E through A, and A to the origin.
    /\ cp = [e \in Edges |->
                IF e \in {CE, <<"E", "A">>, <<"A", "O">>} THEN Fresh(Groups) ELSE Gone]
    /\ front = [r \in Relays |->
                    [cur |-> IF r = "E" THEN "A" ELSE "O", end |-> "open", last |-> None,
                     sw |-> 0]]
    /\ left = FALSE

\* The origin writes a group whole, in order. Frames split only at the hops.
Write(g) ==
    /\ ~org.fin
    /\ org.fr[g] < 0
    /\ IF g = 0 THEN TRUE ELSE org.fr[g - 1] >= 0
    /\ org' = [org EXCEPT !.fr[g] = F]
    /\ UNCHANGED <<alive, deaths, cp, front, left>>

\* The origin ends the track after its last frame.
OriginEnd ==
    /\ ~org.fin
    /\ \A g \in Groups : org.fr[g] = F
    /\ org' = [org EXCEPT !.fin = TRUE]
    /\ UNCHANGED <<alive, deaths, cp, front, left>>

\* The reader at e[2] hands out a group from a copy that holds it, opening its
\* stream to e[1] (`Reader::take`). A fetch is served after the end too.
HandOut(e, g, s) ==
    /\ Lacks(e, g)
    /\ ~cp[e].fin \/ g \in cp[e].fetch
    /\ s \in Srcs(e[2])
    /\ SrcFr(e[2], s, g) >= 0
    /\ cp' = [cp EXCEPT ![e].fr[g] = cp[e].lent[g], ![e].src[g] = s]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* The next frame of a group crosses the hop, up to where the subscription stops it; a
\* fetch asks for the rest.
Frame(e, g) ==
    /\ Short(e, g)
    /\ SrcFr(e[2], cp[e].src[g], g) > cp[e].fr[g]
    /\ g \in cp[e].fetch \/ cp[e].fr[g] < cp[e].stop[g]
    /\ cp' = [cp EXCEPT ![e].fr[g] = @ + 1]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A short group whose copy failed continues on any other copy holding it, and
\* one whose copy stalls while a newer route serves continues on the serving
\* copy, both at the same frame (`Recover::poll`).
Recover(e, g, s) ==
    LET u == e[2] IN
    /\ u # "O"
    /\ Short(e, g)
    /\ Exhausted(e, g)
    /\ front[u].end # "err"
    /\ s \in Holders(u, e, g)
    /\ \/ SrcDead(u, cp[e].src[g], g)
       \/ cp[e].src[g] # front[u].cur /\ s = front[u].cur
    /\ cp' = [cp EXCEPT ![e].src[g] = s]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay asks upstream for a group a reader needs that no copy it holds has or
\* may still land: a backfill for a widened or fetched range, sent where its reads
\* go first (`Fetching`), or a fetch of the rest of a short group, sent to the
\* serving route (`Recover::poll_fetch`).
Ask(u, g) ==
    LET w == front[u].cur
        \* A missing group goes where the relay's reads go first; the rest of a short
        \* one goes to the serving route.
        missing == \E e \in Downs(u) :
                       /\ Lacks(e, g)
                       /\ \A s \in Srcs(u) : SrcFr(u, s, g) < 0
                       /\ ~MayLand(u, g)
        short == \E e \in Downs(u) :
                     /\ Short(e, g)
                     /\ Exhausted(e, g)
                     /\ cp[e].src[g] # w
                     /\ Holders(u, e, g) = {}
    IN
    /\ u \in Relays
    /\ front[u].end # "err"
    /\ \E e0 \in {Answering(u), Serving(u)} :
        /\ \/ e0 = Answering(u) /\ missing
           \/ e0 = Serving(u) /\ short
        /\ Live(e0)
        /\ g \notin cp[e0].ask \cup cp[e0].fetch
        /\ cp[e0].fr[g] = -1
        /\ cp' = [cp EXCEPT ![e0].fetch = @ \cup {g}]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A short group nothing can continue fails (`Recover::poll` gives up): its
\* source failed, no other copy holds it, and no fetch can reach the rest,
\* because the front has no route left, ended with its serving copy gone and
\* no route to move to, or the live serving copy is the source that gave up
\* on the group.
Abort(e, g) ==
    LET u == e[2] IN
    /\ u # "O"
    /\ Short(e, g)
    /\ Exhausted(e, g)
    /\ SrcDead(u, cp[e].src[g], g)
    /\ Holders(u, e, g) = {}
    /\ \/ front[u].end = "err"
       \/ front[u].end = "ok" /\ ~Live(Serving(u)) /\ \A w \in Up(u) : ~Shows(w)
       \/ cp[e].src[g] = front[u].cur /\ Live(Serving(u))
    /\ cp' = [cp EXCEPT ![e].bad = @ \cup {g}]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay with readers subscribes its serving route.
Subscribe(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ \E e \in Downs(u) : Reads(e)
    /\ Routable(front[u].cur)
    /\ ~Held(Serving(u))
    /\ cp' = [cp EXCEPT ![Serving(u)] = Fresh(Demand(u))]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* The front moves to another route: forced off a dead one, or at will while
\* the switches last (`Producer::splice`). A copy it still holds of that route
\* is asked again from the boundary, as is one whose subscription ended under
\* a cap short of the end (a new subscription, `Action::Query`); otherwise a
\* new one is. Every other copy it holds is capped at the boundary at once, its group
\* stopped where the new copy resumes it, so it sends nothing newer; while nothing is held nor handed out, they are dropped. A
\* track that ended moves too, for the group reads and fetches still holding
\* it (`Action::Reroute`): its readers stay on the copy it ended on, so a
\* copy asked for them subscribes nothing, and one still held is left as is.
Switch(u, w) ==
    LET usable == Live(Serving(u)) /\ Routable(front[u].cur)
        e1 == <<u, w>>
        open == front[u].end = "open"
        cap == CapAt(u)
        stop == StopAt(u)
    IN
    /\ u \in Relays
    /\ front[u].end # "err"
    /\ Wanted(u)
    /\ w \in Up(u) /\ w # front[u].cur
    /\ Routable(w)
    /\ usable => front[u].sw < MaxSwitches
    /\ front' = [front EXCEPT ![u].cur = w, ![u].sw = @ + (IF usable THEN 1 ELSE 0)]
    /\ cp' = [e \in Edges |->
                IF e = e1 THEN
                    IF Live(e1) /\ ~cp[e1].fin /\ open
                        THEN Lend(u, Unstopped([cp[e1] EXCEPT !.ask = Demand(u)]))
                    ELSE IF Live(e1) /\ ~Whole(e1) /\ open
                        THEN Lend(u, Unstopped([cp[e1] EXCEPT !.ask = Demand(u), !.fin = FALSE]))
                    ELSE IF Live(e1) THEN Unstopped(cp[e1])
                    ELSE IF open THEN Lend(u, Fresh(Demand(u)))
                    ELSE Fresh({})
                ELSE IF open /\ e[1] = u /\ Live(e) /\ ~cp[e].fin
                    THEN IF cap < 0 /\ HeldAt(u) = {} THEN Gone
                         ELSE Stopped([cp[e] EXCEPT !.ask = {g \in @ : g <= cap}], stop)
                ELSE cp[e]]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* The serving copy completed: the front ends the track (`Action::Finish`).
FrontFinish(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ Live(Serving(u))
    /\ Whole(Serving(u))
    /\ front' = [front EXCEPT ![u].end = "ok", ![u].last = front[u].cur]
    /\ UNCHANGED <<org, alive, deaths, cp, left>>

\* No route is left, not even one still shown: the front fails the track, and every
\* reader with it.
FrontFail(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ ~Live(Serving(u)) \/ ~Routable(front[u].cur)
    /\ \A w \in Up(u) : ~Shows(w)
    /\ front' = [front EXCEPT ![u].end = "err"]
    /\ cp' = [e \in Edges |->
                IF e[2] = u /\ Live(e) THEN [cp[e] EXCEPT !.st = "failed"] ELSE cp[e]]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* A reader ends its subscription cleanly: its track ended at the origin or
\* the relay's front finished, and it handed out all it can.
EndOk(e) ==
    LET u == e[2] IN
    /\ Live(e)
    /\ ~cp[e].fin
    /\ IF u = "O" THEN org.fin ELSE front[u].end = "ok"
    /\ Drained(e)
    /\ cp' = [cp EXCEPT ![e].fin = TRUE]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay lets go of a replaced copy nothing needs: no short group reads from
\* it or could continue on it, and it holds or may land no group a reader
\* lacks (`Reader::let_go`, `Copy::retains`).
LetGo(u, s) ==
    LET e1 == <<u, s>> IN
    /\ u \in Relays
    /\ s \in Up(u)
    /\ Held(e1)
    /\ s # front[u].cur \/ front[u].end = "err"
    \* A finished track keeps the copy it ended on (`Phase::Finished`).
    /\ ~(front[u].end = "ok" /\ s = front[u].last)
    /\ \A e \in Downs(u), g \in Groups :
        /\ Short(e, g) => cp[e].src[g] # s /\ cp[e1].fr[g] < 0 /\ ~MayLand(u, g)
        /\ Lacks(e, g) => cp[e1].fr[g] < 0 /\ ~(Live(e1) /\ g \in cp[e1].ask)
    /\ cp' = [cp EXCEPT ![e1] = Gone]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay nobody reads parks its track, letting go of every copy (`park`). A
\* track that ended is forgotten, so the next reader gets a fresh front: the
\* cache is only an optimization, so the model keeps none.
Park(u) ==
    /\ u \in Relays
    /\ ~Wanted(u)
    /\ \E s \in Up(u) : Held(<<u, s>>)
    /\ cp' = [e \in Edges |-> IF e[1] = u THEN Gone ELSE cp[e]]
    /\ front' = [front EXCEPT ![u].end = IF @ = "ok" THEN "open" ELSE @,
                              ![u].last = None]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* The linger runs out on a relay whose track ended and that nothing reads any
\* more (`Action::Forget`): it is forgotten, so the next reader gets a fresh front.
\* A read still holding the track keeps it, so the read still moves off a dying
\* route (`TrackWeak::is_held`).
Forget(u) ==
    /\ u \in Relays
    /\ front[u].end = "ok"
    /\ ~\E e \in Downs(u) : Reads(e)
    /\ front' = [front EXCEPT ![u].end = "open", ![u].last = None]
    /\ cp' = [e \in Edges |-> IF e[1] = u THEN Gone ELSE cp[e]]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* A relay dies: copies downstream of it fail, and its own vanish. Its route still
\* shows until `Withdraw`.
Die(r) ==
    /\ r \in Mortal
    /\ alive[r] = "up"
    /\ deaths < MaxDeaths
    /\ alive' = [alive EXCEPT ![r] = "shown"]
    /\ deaths' = deaths + 1
    /\ cp' = [e \in Edges |->
                IF e[1] = r THEN Gone
                ELSE IF e[2] = r /\ Live(e) THEN [cp[e] EXCEPT !.st = "failed"]
                ELSE cp[e]]
    /\ UNCHANGED <<org, front, left>>

\* A dead relay's route leaves the table.
Withdraw(r) ==
    /\ alive[r] = "shown"
    /\ alive' = [alive EXCEPT ![r] = "down"]
    /\ UNCHANGED <<org, deaths, cp, front, left>>

\* The client unsubscribes.
Unsubscribe ==
    /\ Leave
    /\ ~left
    /\ left' = TRUE
    /\ cp' = [cp EXCEPT ![CE] = Gone]
    /\ UNCHANGED <<org, alive, deaths, front>>

\* The read is over, or the client left and every relay let go.
Finished ==
    \/ /\ ~left
       /\ \/ cp[CE].st = "failed"
          \/ Whole(CE) /\ \A g \in Groups : cp[CE].fr[g] >= 0
    \/ left /\ \A e \in Edges : ~Held(e)

Done ==
    /\ Finished
    /\ UNCHANGED vars

Next ==
    \/ \E g \in Groups : Write(g)
    \/ OriginEnd
    \/ \E e \in Edges, g \in Groups :
        \/ \E s \in Srcs(e[2]) : HandOut(e, g, s) \/ Recover(e, g, s)
        \/ Frame(e, g) \/ Abort(e, g)
    \/ \E u \in Relays :
        \/ Subscribe(u) \/ FrontFinish(u) \/ FrontFail(u) \/ Park(u) \/ Forget(u)
        \/ \E w \in Up(u) : Switch(u, w) \/ LetGo(u, w)
        \/ \E g \in Groups : Ask(u, g)
    \/ \E e \in Edges : EndOk(e)
    \/ \E r \in Mortal : Die(r) \/ Withdraw(r)
    \/ Unsubscribe
    \/ Done

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)

\* The client's track ends cleanly only with every group handed out.
CleanEndIsComplete == Live(CE) /\ cp[CE].fin => \A g \in Groups : cp[CE].fr[g] >= 0

\* The client's track ends cleanly only after the origin ended it.
CleanEndIsDeclared == cp[CE].fin => org.fin

\* A group the client started fails only once no route is left to continue it: E's
\* track failed, or it finished and no route of E's still shows.
NoSkipWhileHeld == cp[CE].bad # {} => front["E"].end = "err" \/ \A w \in Up("E") : ~Shows(w)

\* A finished read holds every frame of every group, unless E lost every route.
SettledIsWhole ==
    (Finished /\ ~left /\ cp[CE].st = "live" /\ \E w \in Up("E") : Shows(w)) =>
        \A g \in Groups : cp[CE].fr[g] = F

TypeOK ==
    /\ \A e \in Edges :
        /\ cp[e].st \in {"none", "live", "failed"}
        /\ cp[e].bad \subseteq Groups
        /\ \A g \in Groups :
            cp[e].fr[g] \in -1 .. F /\ cp[e].lent[g] \in 0 .. F /\ cp[e].stop[g] \in 1 .. F
    /\ \A r \in Relays : front[r].end \in {"open", "ok", "err"}
    /\ \A r \in Relays : alive[r] \in {"up", "shown", "down"}

=============================================================================
