------------------------------ MODULE WarmSettle ------------------------------
(***************************************************************************)
(* Where each relay pulls one broadcast from, under the warmth rules the   *)
(* simulator in `rs/moq-net/src/route/tests/warm.rs` measures, and whether *)
(* the pulls settle once readers, links, and the publisher stop changing.  *)
(*                                                                         *)
(* Each relay with demand (a reader, or a neighbour pulling through it)    *)
(* pulls through one parent: its route's next hop, or a neighbour with a   *)
(* warm copy it heard of. What a relay tells a neighbour over one session  *)
(* is a register the neighbour sees late: the rank its pull carries, its   *)
(* answer to the neighbour's pull, and its advert. `Deliver` hands the     *)
(* newest value over at any moment, so a relay acts on stale words for as  *)
(* long as TLC likes, skips values, and hears each neighbour in any order. *)
(*                                                                         *)
(* `Rule` picks how a relay chooses:                                       *)
(*  - "order": readers are ranked, by route cost then relay or by relay    *)
(*    alone (`Order`). A relay takes a warm copy only from a neighbour     *)
(*    feeding a reader ranked below every reader it feeds itself, priced  *)
(*    at the link alone (or from the publisher, `Summed`), and heads for   *)
(*    the cheapest such copy or its route whenever either changes.         *)
(*  - "orderless": the same with the rank check removed.                   *)
(*  - "pick": the price-moving pick moq-dev/moq#8903 rejected: a copy is   *)
(*    priced from the publisher, warm only while its sender has demand     *)
(*    besides the asker, and a live pull moves to anything strictly        *)
(*    cheaper, fenced off its own ancestors.                               *)
(*  - "stay": settled pulls, the first cut select.md ports: a relay        *)
(*    chooses when its demand starts, when its route changes, and when its *)
(*    pull is lost or runs through itself, never for a price.              *)
(*                                                                         *)
(* Settling is judged from when the outside events stop (`Read`, `Close`,  *)
(* `Reopen`, each bounded), not from when the registers drain: with weak   *)
(* fairness on every delivery and every relay's reaction, `PullsQuiet`     *)
(* says the pulls eventually stop changing, and `EventuallyFed` that every *)
(* reader whose relay has a route is then fed for good.                    *)
(*                                                                         *)
(* Scope: one broadcast and one publisher, which never moves. The route    *)
(* layer is an oracle: each relay's route is the shortest over the open    *)
(* links at once (`verify-route` and the route proof cover how routes      *)
(* settle). The three words a session carries one way arrive together,    *)
(* though the subscription, its answer, and the advert ride separate      *)
(* streams. No link is marked upstream, so the mark is left out, and there *)
(* is no query. A relay advertises to a neighbour only where the copy,     *)
(* priced from the publisher, beats the neighbour's route (`Useful`).      *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Shape,      \* the mesh, by name: see Links
    Flaky,      \* relays whose link to the publisher's relay may close and reopen
    Readable,   \* relays whose reader may start and stop
    Readers,    \* relays reading at the start
    MaxReads,   \* reader starts and stops, in all
    MaxCloses,  \* link closes and reopens, in all
    Rule,       \* "order", "orderless", "pick", or "stay"
    Order,      \* with "order": "route" or "id"
    Summed      \* with "order": a copy is priced from the publisher, not the link alone

\* Each shape's links, once each as <<a, b, cold, warm>> with a < b; the
\* publisher joins relay 1.
Links ==
    CASE Shape = "drones" ->
            \* Three drones meshed over cheap links, each with a costly uplink.
            {<<1, 2, 10, 4>>, <<1, 3, 10, 4>>, <<1, 4, 10, 4>>,
             <<2, 3, 2, 1>>, <<2, 4, 2, 1>>, <<3, 4, 2, 1>>}
      [] Shape = "pops" ->
            \* Pops on a line from the publisher's, with long links passing over them.
            {<<1, 2, 6, 1>>, <<2, 3, 4, 1>>, <<3, 4, 4, 1>>, <<1, 3, 6, 6>>, <<1, 4, 8, 8>>}
      [] Shape = "mesh503" ->
            \* Churned random mesh 503 where the pick never settled its second
            \* broadcast: its relays 5, 4, 3, 6, 2, and 0 as 1 to 6, over the links
            \* open at the end.
            {<<1, 2, 6, 1>>, <<2, 3, 1, 1>>, <<2, 4, 6, 3>>, <<3, 4, 2, 1>>,
             <<3, 5, 2, 2>>, <<2, 5, 5, 3>>, <<4, 6, 3, 3>>}

N == Cardinality(UNION {{l[1], l[2]} : l \in Links})
Home == 1
Relays == 1 .. N
None == 0
Inf == 100
\* No rank: above every rank a reader has, and no pull at all below every one.
NoRank == 10 * Inf + N + 1
NoPull == -1
Ordered == Rule \in {"order", "orderless"}

Min(S) == CHOOSE x \in S : \A y \in S : x <= y

Pair(a, b) == {a, b}
AllPairs == {Pair(l[1], l[2]) : l \in Links}
\* Each link's prices, and each relay's neighbours, worked out once.
Prices == [p \in AllPairs |-> LET l == CHOOSE l \in Links : Pair(l[1], l[2]) = p IN <<l[3], l[4]>>]
Neighbours == [r \in Relays |-> {n \in Relays : Pair(r, n) \in AllPairs}]
Cold(a, b) == Prices[Pair(a, b)][1]
Warm(a, b) == Prices[Pair(a, b)][2]
Nb(r) == Neighbours[r]

InPath(r, path) == \E i \in 1 .. Len(path) : path[i] = r

NoSig == [cost |-> 0, path |-> <<>>, rank |-> NoRank]
Empty == [pull |-> NoPull,
          ans |-> [kind |-> "none", sig |-> NoSig],
          adv |-> [on |-> FALSE, sig |-> NoSig]]

VARIABLES
    reader,   \* reader[r]: a reader at r
    open,     \* the open links, each as a pair
    route,    \* route[r]: r's next hop toward the publisher and its cold cost
    up,       \* up[r]: the neighbour r pulls through, or None
    nxt,      \* nxt[r]: a move under way: it replaces up[r] once answered
    view,     \* view[r][s]: what r last heard over its session to s
    ok,       \* ok[r]: those of r's pulls answered live once, whose silence is a loss
    chosen,   \* chosen[r]: with "stay", the route r last chose by
    reads,    \* reader starts and stops so far
    closes    \* link closes and reopens so far

vars == <<reader, open, route, up, nxt, view, ok, chosen, reads, closes>>

(***************************************************************************)
(* The route layer, as an oracle over the open links.                      *)
(***************************************************************************)
OpenNb(r, o) == {n \in Nb(r) : Pair(r, n) \in o}

RECURSIVE Dist(_, _)
Dist(k, o) ==
    IF k = 0 THEN [r \in Relays |-> IF r = Home THEN 0 ELSE Inf]
    ELSE LET d == Dist(k - 1, o) IN
         [r \in Relays |-> IF r = Home THEN 0
                           ELSE Min({d[r]} \cup {Cold(r, n) + d[n] : n \in OpenNb(r, o)})]

\* Each relay's next hop, the lowest of equal ones, and its cost.
Routes(o) ==
    LET d == Dist(N, o) IN
    [r \in Relays |->
        IF r = Home THEN <<None, 0>>
        ELSE IF d[r] >= Inf THEN <<None, Inf>>
        ELSE <<Min({n \in OpenNb(r, o) : Cold(r, n) + d[n] = d[r]}), d[r]>>]

Hop(r) == route[r][1]
Reach(r) == r = Home \/ route[r][2] < Inf
IsOpen(a, b) == Pair(a, b) \in open

(***************************************************************************)
(* What a relay knows of its own pull and its neighbours'.                 *)
(***************************************************************************)
Down(r) == {c \in Nb(r) : IsOpen(r, c) /\ view[r][c].pull # NoPull}
Demand(r) == reader[r] \/ Down(r) # {}
Others(r, c) == reader[r] \/ Down(r) \ {c} # {}

Own(r) ==
    IF ~reader[r] THEN NoRank
    ELSE IF Order = "id" THEN r
    ELSE route[r][2] * (N + 1) + r

\* The lowest rank among the readers r feeds, leaving out c's.
RankOf(r, c) ==
    IF ~Ordered THEN 0
    ELSE Min({Own(r)} \cup {view[r][d].pull : d \in Down(r) \ {c}})

Ans(r, s) == view[r][s].ans
\* How r's pull through s stands: "live", "self" (its path names r), "lost", or
\* "pending". A parent answers once it is fed or stuck, and every change after, so
\* one that answered live and then has nothing to offer has lost its upstream.
State(r, s) ==
    CASE Ans(r, s).kind = "live" /\ ~InPath(r, Ans(r, s).sig.path) -> "live"
      [] Ans(r, s).kind = "live" -> "self"
      [] Ans(r, s).kind = "lost" \/ s \in ok[r] -> "lost"
      [] OTHER -> "pending"

Live(r) == r = Home \/ (up[r] # None /\ State(r, up[r]) = "live")
Sig(r) == IF r = Home THEN NoSig ELSE Ans(r, up[r]).sig

\* What r offers c, while r is fed.
Offer(r, c) ==
    LET link == IF r = Home THEN 0
                ELSE IF Ordered \/ Others(r, c) THEN Warm(up[r], r)
                ELSE Cold(up[r], r)
    IN [cost |-> Sig(r).cost + link,
        path |-> Append(Sig(r).path, r),
        rank |-> IF Ordered THEN RankOf(r, c) ELSE NoRank]

Stuck(r) == r # Home /\ nxt[r] = None /\ (up[r] = None \/ State(r, up[r]) = "lost")

Answer(r, c) ==
    IF c \notin Down(r) THEN Empty.ans
    ELSE IF Live(r) THEN [kind |-> "live", sig |-> Offer(r, c)]
    ELSE IF Stuck(r) THEN [kind |-> "lost", sig |-> NoSig]
    ELSE [kind |-> "wait", sig |-> NoSig]

\* Whether c could take r's copy, priced from the publisher whatever c prices it at:
\* below c's route, or, when c's route runs through r, below r's own route.
Useful(r, c) ==
    IF Hop(c) = r THEN Offer(r, c).cost < route[r][2]
    ELSE Offer(r, c).cost + Cold(r, c) <= route[c][2]

Advert(r, c) ==
    IF Live(r) /\ Others(r, c) /\ Useful(r, c) THEN [on |-> TRUE, sig |-> Offer(r, c)]
    ELSE Empty.adv

\* What r tells c now: the rank its pull through c carries, its answer, its advert.
Pub(r, c) ==
    [pull |-> IF up[r] = c \/ nxt[r] = c THEN RankOf(r, c) ELSE NoPull,
     ans |-> Answer(r, c),
     adv |-> Advert(r, c)]

(***************************************************************************)
(* What a relay chooses.                                                   *)
(***************************************************************************)
\* The warm copies r heard of: adverts, and with "order" its parent's answer.
Heard(r, u) ==
    {<<s, view[r][s].adv.sig>> : s \in {s \in OpenNb(r, open) : view[r][s].adv.on}}
    \cup (IF Ordered /\ u # None /\ State(r, u) = "live" /\ ~view[r][u].adv.on
          THEN {<<u, Ans(r, u).sig>>} ELSE {})

CopyCost(r, s, sig) ==
    IF Ordered /\ ~Summed THEN Cold(s, r) ELSE sig.cost + Cold(s, r)

\* The pick fences off the relays on its own live pull's path.
Ancestors(r, u) ==
    IF Rule = "pick" /\ u # None /\ State(r, u) = "live"
    THEN {Ans(r, u).sig.path[i] : i \in 1 .. Len(Ans(r, u).sig.path)}
    ELSE {}

Options(r, u) ==
    LET mine == RankOf(r, None)
        routes == IF Hop(r) # None /\ Hop(r) \notin Down(r)
                  THEN {[via |-> Hop(r), cost |-> route[r][2], copy |-> FALSE]}
                  ELSE {}
        copies == {[via |-> h[1], cost |-> CopyCost(r, h[1], h[2]), copy |-> TRUE] :
                   h \in {h \in Heard(r, u) :
                            /\ h[1] \notin Down(r)
                            /\ h[1] \notin Ancestors(r, u)
                            /\ ~InPath(r, h[2].path)
                            /\ Rule = "order" => h[2].rank < mine}}
    IN routes \cup copies

\* The cheaper, a warm copy before the route, then the lower relay.
Better(a, b) ==
    \/ a.cost < b.cost
    \/ a.cost = b.cost /\ a.copy /\ ~b.copy
    \/ a.cost = b.cost /\ a.copy = b.copy /\ a.via < b.via
Best(opts) == CHOOSE o \in opts : \A p \in opts : p = o \/ Better(o, p)
Vias(opts) == {o.via : o \in opts}

\* Head for t, keeping u until the move is answered.
Toward(u, x, t) ==
    IF t = u THEN <<u, None>> ELSE IF t = x THEN <<u, x>> ELSE <<u, t>>

\* The pull and move r's rule picks, from the pull u and move x it has once a
\* move that was answered took effect.
Decide(r, u, x) ==
    LET opts == Options(r, u)
        best == IF opts = {} THEN None ELSE Best(opts).via
        st == IF u = None THEN "none" ELSE State(r, u)
        own == IF st = "live" THEN Ans(r, u).sig.cost + Warm(u, r) ELSE Inf
    IN
    CASE st = "none" -> <<best, None>>
      [] Ordered ->
           (CASE st = "live" /\ best # None ->
                    IF Summed /\ u \in Vias(opts) /\ Best(opts).cost >= own
                    THEN Toward(u, x, u) ELSE Toward(u, x, best)
              [] st = "pending" /\ best = u -> <<u, x>>
              [] OTHER -> <<best, None>>)
      [] Rule = "pick" ->
           (CASE st = "pending" ->
                    LET cur == {o \in opts : o.via = u} IN
                    IF best = None THEN <<None, None>>
                    ELSE IF best # u /\ (cur = {} \/ \A o \in cur : Best(opts).cost < o.cost)
                    THEN <<best, IF x = best THEN None ELSE x>> ELSE <<u, x>>
              [] st = "live" ->
                    LET moving == IF x # None /\ \E o \in opts : o.via = x /\ o.cost <= own
                                  THEN x ELSE None
                    IN IF best # None /\ Best(opts).cost < own THEN Toward(u, x, best)
                       ELSE IF moving # None THEN Toward(u, x, moving)
                       ELSE Toward(u, x, u)
              [] OTHER -> Toward(u, x, IF best = None THEN u ELSE best))
      [] OTHER -> \* "stay"
           (CASE st = "pending" -> IF u \in Vias(opts) THEN <<u, x>> ELSE <<None, None>>
              [] st = "live" ->
                    IF chosen[r] # route[r] /\ best # None /\ best # u /\ Best(opts).cost < own
                    THEN Toward(u, x, best)
                    ELSE Toward(u, x, IF x # None THEN x ELSE u)
              [] OTHER -> <<best, None>>)

\* r's pull and move once it has reacted to what it knows.
Next1(r) ==
    IF ~Demand(r) THEN <<None, None>>
    ELSE
        LET x0 == nxt[r]
            done == x0 # None /\ State(r, x0) = "live"
            drop == x0 # None /\ State(r, x0) \in {"lost", "self"}
            u == IF done THEN x0 ELSE up[r]
            x == IF done \/ drop THEN None ELSE x0
        IN Decide(r, u, x)

\* With "stay", the route a relay chose by: updated whenever it chooses afresh
\* or its route changed while its pull was answered.
Chose(r, pulls) ==
    IF Rule # "stay" THEN chosen[r]
    ELSE IF pulls[1] = None THEN <<None, Inf>>
    ELSE IF up[r] = None \/ (pulls[1] # up[r] /\ pulls[2] = None) THEN route[r]
    ELSE IF State(r, up[r]) # "pending" THEN route[r]
    ELSE chosen[r]

(***************************************************************************)
(* Steps.                                                                  *)
(***************************************************************************)
Init ==
    /\ reader = [r \in Relays |-> r \in Readers]
    /\ open = AllPairs
    /\ route = Routes(AllPairs)
    /\ up = [r \in Relays |-> None]
    /\ nxt = [r \in Relays |-> None]
    /\ view = [r \in Relays |-> [s \in Relays |-> Empty]]
    /\ ok = [r \in Relays |-> {}]
    /\ chosen = [r \in Relays |-> <<None, Inf>>]
    /\ reads = 0
    /\ closes = 0

\* A relay reacts to what it knows, when that changes its pull.
React(r) ==
    /\ r # Home
    /\ LET pulls == Next1(r)
           c == Chose(r, pulls)
       IN /\ <<pulls[1], pulls[2], c>> # <<up[r], nxt[r], chosen[r]>>
          /\ up' = [up EXCEPT ![r] = pulls[1]]
          /\ nxt' = [nxt EXCEPT ![r] = pulls[2]]
          /\ chosen' = [chosen EXCEPT ![r] = c]
          \* A pull replaced outright by one through the same neighbour starts afresh.
          /\ ok' = [ok EXCEPT ![r] = {s \in @ : s \in {pulls[1], pulls[2]} /\ State(r, s) # "lost"}]
    /\ UNCHANGED <<reader, open, route, view, reads, closes>>

\* r hears what s tells it now.
Deliver(s, r) ==
    /\ s \in Nb(r)
    /\ IsOpen(s, r)
    /\ view[r][s] # Pub(s, r)
    /\ view' = [view EXCEPT ![r][s] = Pub(s, r)]
    /\ ok' = [ok EXCEPT ![r] =
                IF s \in {up[r], nxt[r]} /\ Pub(s, r).ans.kind = "live" THEN @ \cup {s} ELSE @]
    /\ UNCHANGED <<reader, open, route, up, nxt, chosen, reads, closes>>

Read(r) ==
    /\ r \in Readable
    /\ reads < MaxReads
    /\ reads' = reads + 1
    /\ reader' = [reader EXCEPT ![r] = ~@]
    /\ UNCHANGED <<open, route, up, nxt, view, ok, chosen, closes>>

\* The session over a link closes, and both ends forget what it carried.
Close(a, b) ==
    /\ a = Home /\ b \in Flaky
    /\ IsOpen(a, b)
    /\ closes < MaxCloses
    /\ closes' = closes + 1
    /\ open' = open \ {Pair(a, b)}
    /\ route' = Routes(open')
    /\ LET other(r) == IF r = a THEN b ELSE a IN
       /\ up' = [r \in Relays |->
                   IF r \in {a, b} /\ up[r] = other(r)
                   THEN (IF nxt[r] = other(r) THEN None ELSE nxt[r])
                   ELSE up[r]]
       /\ nxt' = [r \in Relays |->
                   IF r \in {a, b} /\ (up[r] = other(r) \/ nxt[r] = other(r)) THEN None
                   ELSE nxt[r]]
    /\ view' = [view EXCEPT ![a][b] = Empty, ![b][a] = Empty]
    /\ ok' = [ok EXCEPT ![a] = @ \ {b}, ![b] = @ \ {a}]
    /\ UNCHANGED <<reader, chosen, reads>>

Reopen(a, b) ==
    /\ a = Home /\ b \in Flaky
    /\ ~IsOpen(a, b)
    /\ closes < MaxCloses
    /\ closes' = closes + 1
    /\ open' = open \cup {Pair(a, b)}
    /\ route' = Routes(open')
    /\ UNCHANGED <<reader, up, nxt, view, ok, chosen, reads>>

Outside ==
    \/ \E r \in Relays : Read(r)
    \/ \E r \in Flaky : Close(Home, r) \/ Reopen(Home, r)

Next ==
    \/ Outside
    \/ \E r \in Relays : React(r)
    \/ \E r \in Relays, s \in Relays : Deliver(s, r)

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A r \in Relays : WF_vars(React(r))
    /\ \A r \in Relays, s \in Relays : WF_vars(Deliver(s, r))

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)
TypeOK ==
    /\ reader \in [Relays -> BOOLEAN]
    /\ up \in [Relays -> Relays \cup {None}]
    /\ nxt \in [Relays -> Relays \cup {None}]
    /\ \A r \in Relays : up[r] # None => up[r] \in Nb(r)
    /\ \A r \in Relays : nxt[r] # None => nxt[r] \in Nb(r) /\ nxt[r] # up[r]
    /\ \A r \in Relays : ok[r] \subseteq {up[r], nxt[r]} \ {None}

\* r's pulls lead to the publisher, each held by its parent and answered live.
RECURSIVE FedFrom(_, _)
FedFrom(r, k) ==
    IF r = Home THEN TRUE
    ELSE IF k = 0 THEN FALSE
    ELSE LET p == up[r] IN
         /\ p # None
         /\ IsOpen(r, p)
         /\ State(r, p) = "live"
         /\ view[p][r].pull # NoPull
         /\ FedFrom(p, k - 1)

AllFed == \A r \in Relays : reader[r] /\ Reach(r) => FedFrom(r, N)

\* Once the outside events stop, the pulls stop changing.
PullsQuiet == <>[][UNCHANGED <<up, nxt>>]_vars

\* Once they stop, every reader with a route is fed for good.
EventuallyFed == <>[]AllFed
==============================================================================
