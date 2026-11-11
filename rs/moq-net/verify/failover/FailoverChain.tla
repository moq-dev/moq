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
(*  - A route change is one `Switch` (`verify-route` checks which next hop *)
(*    routing picks; here any live upstream may win).                      *)
(*  - A switch asks the new copy from the reader's newest group, or from   *)
(*    the lowest gap below it (`Reader::sync`, `backfill_gaps`,            *)
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
(* route lives, after the track ended too: the skill's open findings are   *)
(* where the code does not yet.                                            *)
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
    alive,   \* which relays are up
    deaths,  \* relays that died
    cp,      \* cp[e]: the copy at e[1] of the track e[2] serves, and e[2]'s reader for it
    front,   \* front[r]: the route r serves from, how its track ended, switches made
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
(***************************************************************************)

Fresh(ask) ==
    [st |-> "live", ask |-> ask, fetch |-> {}, fr |-> [g \in Groups |-> -1],
     bad |-> {}, fin |-> FALSE, src |-> [g \in Groups |-> None]]

Gone ==
    [st |-> "none", ask |-> {}, fetch |-> {}, fr |-> [g \in Groups |-> -1],
     bad |-> {}, fin |-> FALSE, src |-> [g \in Groups |-> None]]

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

\* What a switch asks the new copy for: from the reader's newest group handed
\* out (`sync`), or from the lowest group below it the reader lacks
\* (`backfill_gaps`), or everything when nothing was handed out yet.
ResumeAsk(e) ==
    LET got == {g \in Groups : cp[e].fr[g] >= 0}
        newest == Max(got)
        gaps == {g \in cp[e].ask : g < newest /\ cp[e].fr[g] = -1}
        from == IF got = {} THEN 0 ELSE IF gaps # {} THEN Min(gaps) ELSE newest
    IN {g \in cp[e].ask : g >= from}

\* What a copy at u asks: the union of its readers' demand (`Producer::subscription`).
Demand(u) == UNION {ResumeAsk(e) : e \in {x \in Downs(u) : Live(x)}}
Wanted(u) == \E e \in Downs(u) : Live(e)

Serving(u) == <<u, front[u].cur>>
Routable(w) == IF w = "O" THEN TRUE ELSE alive[w] /\ front[w].end # "err"

\* A complete copy (`TrackState::is_complete` once every group below the end
\* finished): its upstream ended it, and every group it asked holds every frame.
Whole(e) ==
    /\ cp[e].fin
    /\ \A g \in cp[e].ask : cp[e].fr[g] = F \/ g \in cp[e].bad

\* A held copy at u may still land g: it is live, asked for g, and is not done.
MayLand(u, g) ==
    \E s \in Up(u) :
        /\ Live(<<u, s>>)
        /\ g \in cp[<<u, s>>].ask
        /\ cp[<<u, s>>].fr[g] = -1
        /\ ~cp[<<u, s>>].fin

\* The reader at u has handed out all it can: every group its copy asks for is
\* out, or no copy at u holds it or may still land it (`poll_end`).
Drained(e) ==
    LET u == e[2] IN
    \A g \in cp[e].ask :
        \/ cp[e].fr[g] >= 0
        \/ /\ \A s \in Srcs(u) : SrcFr(u, s, g) < 0
           /\ ~MayLand(u, g)

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
    /\ alive = [r \in Relays |-> TRUE]
    /\ deaths = 0
    \* The client subscribed through E, E through A, and A to the origin.
    /\ cp = [e \in Edges |->
                IF e \in {CE, <<"E", "A">>, <<"A", "O">>} THEN Fresh(Groups) ELSE Gone]
    /\ front = [r \in Relays |->
                    [cur |-> IF r = "E" THEN "A" ELSE "O", end |-> "open", sw |-> 0]]
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
    /\ cp' = [cp EXCEPT ![e].fr[g] = 0, ![e].src[g] = s]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* The next frame of a group crosses the hop.
Frame(e, g) ==
    /\ Short(e, g)
    /\ SrcFr(e[2], cp[e].src[g], g) > cp[e].fr[g]
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
       \/ cp[e].src[g] # front[u].cur /\ s = front[u].cur /\ front[u].end = "open"
    /\ cp' = [cp EXCEPT ![e].src[g] = s]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay asks its serving route for a group a reader needs that no copy it
\* holds has or may still land: a backfill for a widened or fetched range, or a
\* fetch of the rest of a short group (`Recover::poll_fetch`).
Ask(u, g) ==
    LET w == front[u].cur
        e0 == Serving(u)
    IN
    /\ u \in Relays
    /\ front[u].end # "err"
    /\ Live(e0)
    /\ g \notin cp[e0].ask \cup cp[e0].fetch
    /\ cp[e0].fr[g] = -1
    /\ \E e \in Downs(u) :
        \/ /\ Lacks(e, g)
           /\ \A s \in Srcs(u) : SrcFr(u, s, g) < 0
           /\ ~MayLand(u, g)
        \/ /\ Short(e, g)
           /\ Exhausted(e, g)
           /\ cp[e].src[g] # w
           /\ Holders(u, e, g) = {}
    /\ cp' = [cp EXCEPT ![e0].fetch = @ \cup {g}]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A short group nothing can continue fails (`Recover::poll` gives up): its
\* source failed, no other copy holds it, and no fetch can reach the rest,
\* because the front has no route left, ended with its serving copy gone, or
\* the live serving copy is the source that gave up on the group.
Abort(e, g) ==
    LET u == e[2] IN
    /\ u # "O"
    /\ Short(e, g)
    /\ Exhausted(e, g)
    /\ SrcDead(u, cp[e].src[g], g)
    /\ Holders(u, e, g) = {}
    /\ \/ front[u].end = "err"
       \/ front[u].end = "ok" /\ ~Live(Serving(u))
       \/ cp[e].src[g] = front[u].cur /\ Live(Serving(u))
    /\ cp' = [cp EXCEPT ![e].bad = @ \cup {g}]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* A relay with readers subscribes its serving route.
Subscribe(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ Wanted(u)
    /\ Routable(front[u].cur)
    /\ ~Held(Serving(u))
    /\ cp' = [cp EXCEPT ![Serving(u)] = Fresh(Demand(u))]
    /\ UNCHANGED <<org, alive, deaths, front, left>>

\* The front moves to another route: forced off a dead one, or at will while
\* the switches last (`Producer::splice`). A copy it still holds of that route
\* is asked again from where the readers got to; otherwise a new one is.
Switch(u, w) ==
    LET usable == Live(Serving(u)) /\ Routable(front[u].cur)
        e1 == <<u, w>>
    IN
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ Wanted(u)
    /\ w \in Up(u) /\ w # front[u].cur
    /\ Routable(w)
    /\ usable => front[u].sw < MaxSwitches
    /\ front' = [front EXCEPT ![u].cur = w, ![u].sw = @ + (IF usable THEN 1 ELSE 0)]
    /\ cp' = [cp EXCEPT ![e1] =
                IF Live(e1) /\ ~cp[e1].fin THEN [@ EXCEPT !.ask = Demand(u)]
                ELSE IF Live(e1) THEN @
                ELSE Fresh(Demand(u))]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* The serving copy completed: the front ends the track (`Action::Finish`).
FrontFinish(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ Live(Serving(u))
    /\ Whole(Serving(u))
    /\ front' = [front EXCEPT ![u].end = "ok"]
    /\ UNCHANGED <<org, alive, deaths, cp, left>>

\* No route is left: the front fails the track, and every reader with it.
FrontFail(u) ==
    /\ u \in Relays
    /\ front[u].end = "open"
    /\ ~Live(Serving(u)) \/ ~Routable(front[u].cur)
    /\ \A w \in Up(u) : ~Routable(w)
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
    /\ front' = [front EXCEPT ![u].end = IF @ = "ok" THEN "open" ELSE @]
    /\ UNCHANGED <<org, alive, deaths, left>>

\* A relay dies: copies downstream of it fail, and its own vanish.
Die(r) ==
    /\ r \in Mortal
    /\ alive[r]
    /\ deaths < MaxDeaths
    /\ alive' = [alive EXCEPT ![r] = FALSE]
    /\ deaths' = deaths + 1
    /\ cp' = [e \in Edges |->
                IF e[1] = r THEN Gone
                ELSE IF e[2] = r /\ Live(e) THEN [cp[e] EXCEPT !.st = "failed"]
                ELSE cp[e]]
    /\ UNCHANGED <<org, front, left>>

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
        \/ Subscribe(u) \/ FrontFinish(u) \/ FrontFail(u) \/ Park(u)
        \/ \E w \in Up(u) : Switch(u, w) \/ LetGo(u, w)
        \/ \E g \in Groups : Ask(u, g)
    \/ \E e \in Edges : EndOk(e)
    \/ \E r \in Mortal : Die(r)
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

\* A group the client started fails only once no route is left to continue it.
NoSkipWhileHeld == cp[CE].bad # {} => front["E"].end = "err"

\* A finished read holds every frame of every group, unless E lost every route.
SettledIsWhole ==
    (Finished /\ ~left /\ cp[CE].st = "live") =>
        \A g \in Groups : cp[CE].fr[g] = F

TypeOK ==
    /\ \A e \in Edges :
        /\ cp[e].st \in {"none", "live", "failed"}
        /\ cp[e].bad \subseteq Groups
        /\ \A g \in Groups : cp[e].fr[g] \in -1 .. F
    /\ \A r \in Relays : front[r].end \in {"open", "ok", "err"}

=============================================================================
