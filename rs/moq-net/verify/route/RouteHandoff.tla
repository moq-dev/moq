---------------------------- MODULE RouteHandoff ----------------------------
(***************************************************************************)
(* The lite-07 route rule the routes epic's Plan item 27 picked, for one   *)
(* publisher and two prefixes, as Router in                               *)
(* rs/moq-net/src/route/path.rs runs it, with a route per scope.            *)
(*                                                                         *)
(* A route is keyed by the prefix it covers: "root", or "a" inside it. A   *)
(* relay in `Scoped` asks every neighbour for "a" alone, so a stream to it *)
(* carries one route, keyed "a". A route names every relay it crossed,     *)
(* each with the session that relay learned it on, and a relay uses the    *)
(* best live route whose path does not name it: one crossing no draining   *)
(* session, then the highest lowest link priority, then the cheapest. An   *)
(* offer carries its drain; its priority and cost follow from its path.    *)
(* There is no distance, seqno, ROUTE_REQUEST, or echo. Instead:           *)
(*                                                                         *)
(* - A relay that loses a session names that hop in one DEAD message per   *)
(*   neighbour, listing which of its routes through it moved to another    *)
(*   path, and every relay drops every route through the hop.             *)
(* - A relay with no route left holds its claims, unavailable, until each  *)
(*   neighbour that learned the route from it has answered.               *)
(* - A closed session casts doubt on the node at its far end: no relay     *)
(*   moves to a route through it until a timer lifts the doubt. A route    *)
(*   that begins at it is exempt: it cannot loop through its own end. The  *)
(*   timer is the step `Lift`, which may fire at any moment.              *)
(*                                                                         *)
(* Each node's every entry point is one atomic step ending in `Update`,    *)
(* as in the router. Messages travel on one FIFO per direction. A          *)
(* session opens and closes atomically, dropping what was in flight, and   *)
(* each opening is a new session with a new hop. A claim's attributes are  *)
(* the revision that last set them.                                        *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Nodes,        \* relays and the publisher; ordered as `select` breaks ties
    Origin,       \* the publisher
    Links,        \* sessions, as 2-element sets of nodes
    Dear,         \* the links that cost 2; the rest cost 1
    Ring,         \* relays round a ring in number order: each one's link to the
                  \* next costs 1 there and 2 at the next, overriding `Dear`
    Preferred,    \* the links both ends give priority 1; the rest have 0
    Leaving,      \* nodes that may send GOAWAY, once on each session
    Scoped,       \* relays that ask every neighbour for prefix "a" alone
    Claims,       \* claim paths
    Narrow,       \* the claims inside prefix "a"
    MaxRev,       \* the publisher's claim changes
    MaxFlaps,     \* session opens and closes
    Patient       \* TRUE: a doubt lifts only once no news about its node is in flight

Relays == Nodes \ {Origin}
\* The links whose sessions open and close. A config may narrow them to the
\* publisher's own (`OwnLinks`) or to one of them (`FirstLink`).
Flaky == Links
OwnLinks == {l \in Links : Origin \in l}
\* The publisher's link to the lowest relay.
FirstLink == {{Origin, CHOOSE r \in Relays : \A x \in Relays : r <= x}}

Cost == [l \in Links |-> IF l \in Dear THEN 2 ELSE 1]
\* The relay after n round `Ring`, the lowest after the highest.
Succ(n) == LET after == {m \in Ring : m > n}
               from == IF after # {} THEN after ELSE Ring
           IN CHOOSE m \in from : \A k \in from : m <= k
\* What node n's link to m costs n: links round the ring cost by direction.
CostAt(n, m) == IF n \in Ring /\ m \in Ring /\ m = Succ(n) THEN 1
                ELSE IF n \in Ring /\ m \in Ring /\ n = Succ(m) THEN 2
                ELSE Cost[{n, m}]

\* A claim's attributes: the revision that last set it, or None when absent.
None == -1
\* A view's floor: a revision, None for no floor yet, or Absent for no view.
Absent == -2

Keys == {"root", "a"}
\* The keys in the order a node sends them in.
KeySeq == <<"root", "a">>

\* Whether a route keyed `k` covers claim `c`.
Covers(k, c) == k = "root" \/ c \in Narrow
\* The key whose view a node shows `c` from: the narrowest that covers it.
KeyOf(c) == IF c \in Narrow THEN "a" ELSE "root"
\* What `n` asks its neighbours for.
Ask(n) == IF n \in Scoped THEN "a" ELSE "root"
\* The keys a stream to `p` carries.
Streams(p) == IF p \in Scoped THEN {"a"} ELSE Keys

NoItems == [c \in Claims |-> None]
Restrict(items, k) == [c \in Claims |-> IF Covers(k, c) THEN items[c] ELSE None]
EmptyView == [floor |-> None, items |-> NoItems]
NoView == [floor |-> Absent, items |-> NoItems]

\* A hop is <<node, peer, generation>>: the node, and the session to `peer` of
\* that generation it learned the route on. The publisher names itself.
OHop == <<Origin, Origin, 0>>

\* A route a neighbour offered (`Heard`). `state` is "live", "dead" (its path
\* holds a hop this node knows is dead, and the neighbour has not said what
\* became of it), or "held".
NoHeard == [on |-> FALSE, path |-> <<>>, drain |-> FALSE, state |-> "live", rev |-> None, items |-> NoItems]
\* The route a node uses for a key (`Route`). Held: `path` is the last live
\* route's, and `next` is the neighbour it holds under, or None on its own account.
NoRoute == [on |-> FALSE, held |-> FALSE, next |-> None, path |-> <<>>, drain |-> FALSE]
\* What a node last sent a neighbour for a key (`Sent`). `src` is the key of
\* the node's own route the offer reflects, kept after an end.
NoSent(k) == [on |-> FALSE, held |-> FALSE, path |-> <<>>, drain |-> FALSE, rev |-> None, items |-> NoItems, src |-> k]
NotShown == [item |-> None, avail |-> FALSE]

\* Messages: one record shape so TLC can compare them.
Blank == [t |-> "", k |-> "root", path |-> <<>>, drain |-> FALSE, rev |-> None, items |-> NoItems,
          hop |-> OHop, moved |-> {}, gone |-> FALSE, node |-> None]
OfferMsg(k, path, drain) == [Blank EXCEPT !.t = "offer", !.k = k, !.path = path, !.drain = drain]
HoldMsg(k) == [Blank EXCEPT !.t = "hold", !.k = k]
EndMsg(k) == [Blank EXCEPT !.t = "end", !.k = k]
ClaimsMsg(k, v) == [Blank EXCEPT !.t = "claims", !.k = k, !.rev = v.floor, !.items = v.items]
DeadMsg(hop, moved, gone) ==
    [Blank EXCEPT !.t = "dead", !.hop = hop, !.moved = moved, !.gone = gone]
\* The DEAD message's DRAIN entry: `node` sent GOAWAY.
DrainMsg(node) == [Blank EXCEPT !.t = "drain", !.node = node]

VARIABLES
    up,          \* open sessions
    gen,         \* gen[l]: the generation of the session on link l
    q,           \* q[<<m, n>>]: messages from m to n, in order
    rev,         \* the publisher's claim revision
    own,         \* the publisher's claims
    hist,        \* hist[r + 1]: the publisher's claims at revision r
    st,          \* st[n]: node n's state for the publisher
    ended,       \* ended[n][c]: the item n's reader last saw END for, until n forgets c
    revivals,    \* reader STARTs of an item the reader saw END for
    falseEnds,   \* reader ENDs while the publisher still has that item
    regressions, \* reader UPDATEs to an older item
    unstable,    \* claim changes a path change or a lifted doubt alone made
    learnt,      \* learnt[n]: every hop n was ever told is dead
    flaps,
    burst,       \* messages sent since the last quiet point
    churn        \* external changes since the last quiet point

vars == <<up, gen, q, rev, own, hist, st, ended, revivals, falseEnds, regressions,
          unstable, learnt, flaps, burst, churn>>

Dirs == {<<m, n>> \in Nodes \X Nodes : {m, n} \in Links}
Peers(n, ups) == {m \in Nodes : {n, m} \in ups}

-----------------------------------------------------------------------------
(* Paths.                                                                  *)

Names(path, n) == \E i \in DOMAIN path : path[i][1] = n
HasHop(path, h) == \E i \in DOMAIN path : path[i] = h
DeadIn(path, D) == \E i \in DOMAIN path : path[i] \in D
\* The relay that offered `path` learned it from `n`.
Child(path, n) == Len(path) >= 2 /\ path[Len(path) - 1][1] = n
\* What the route costs the last node on its path.
PathCost(path) ==
    LET F[i \in 0..Len(path)] ==
          IF i <= 1 THEN 0 ELSE F[i - 1] + CostAt(path[i][1], path[i - 1][1])
    IN F[Len(path)]
\* The lowest priority of a link on the path: each hop's node and the peer it
\* learned the route from. The publisher's own hop crosses no link.
PathPrio(path) == IF \E i \in DOMAIN path : i > 1 /\ {path[i][1], path[i][2]} \notin Preferred
                  THEN 0 ELSE 1
\* How a route ranks, lower first: drain, then priority, then cost. Extending a
\* path never improves the first two and strictly raises the cost.
Rank(path, drain) == <<IF drain THEN 1 ELSE 0, 1 - PathPrio(path), PathCost(path)>>
Below(a, b) == \/ a[1] < b[1]
               \/ a[1] = b[1] /\ a[2] < b[2]
               \/ a[1] = b[1] /\ a[2] = b[2] /\ a[3] < b[3]

Live(r) == r.on /\ ~r.held

-----------------------------------------------------------------------------
(* Selection on node n's state s (path.rs: `select`, `pending`, `hopes`). *)

\* The path crosses a relay in doubt. Its first node, where the route begins,
\* does not count: a route cannot loop through its own end.
RelayDoubted(s, path) == \E i \in DOMAIN path : i > 1 /\ path[i][1] \in s.doubt
\* Doubt keeps a node off a route through a doubted relay, not off one that
\* begins at a doubted publisher.
Doubted(s, path) == RelayDoubted(s, path)
\* The path crosses a node this node knows sent GOAWAY.
Drains(s, path) == \E i \in DOMAIN path : path[i][1] \in s.draining

\* A route a neighbour offered that this node may use: live, and its path does
\* not name this node.
Usable(n, s, m, k) ==
    s.rx[m][k].on /\ s.rx[m][k].state = "live" /\ ~Names(s.rx[m][k].path, n)

\* The key of this node's own route an offer of key k to p reflects. A stream
\* to a scoped relay carries one route, "a": the node's route for "a" where it
\* has a live one not naming the receiver, else its root route, which covers "a".
\* A narrower path cannot outrank a better covering path: that preference can
\* make neighbours oscillate between scopes. Equal ranks prefer the narrow key.
Cheapest(s, ks) ==
    LET R(k) == Rank(s.rt[k].path, s.rt[k].drain)
    IN CHOOSE k \in ks : \A j \in ks :
        \/ Below(R(k), R(j))
        \/ R(k) = R(j) /\ (k = "a" \/ j = "root")

SrcKey(s, p, k) ==
    IF p \notin Scoped THEN k
    ELSE LET live == {j \in Keys : Live(s.rt[j])}
             usable == {j \in live : ~Names(s.rt[j].path, p)}
         IN IF usable # {} THEN Cheapest(s, usable)
            ELSE IF live # {} THEN Cheapest(s, live)
            ELSE IF s.rt["a"].on THEN "a" ELSE "root"

\* A neighbour that learned this node's route for k has yet to answer its loss.
Pending(n, s, k) ==
    \E p \in Nodes : \E j \in Keys :
        /\ s.rx[p][j].on
        /\ s.rx[p][j].state # "held"
        /\ Child(s.rx[p][j].path, n)
        /\ s.tx[p][j].src = k

\* A neighbour offers a route that only doubt keeps this node off.
Hopes(n, s, k) == \E m \in Nodes : Usable(n, s, m, k) /\ Doubted(s, s.rx[m][k].path)

\* `select` for one key: the best-ranked usable route, staying on the next hop
\* among equals, then the lowest key. Doubt keeps a node from moving to a route,
\* not on the one it uses. With none left, hold or let go.
SelectKey(n, s, k, gens) ==
    LET was == s.rt[k]
        current == IF was.on THEN was.next ELSE None
        using == IF Live(was) THEN was.next ELSE None
        C == {m \in Nodes : Usable(n, s, m, k) /\ (m = using \/ ~Doubted(s, s.rx[m][k].path))}
        Path(m) == Append(s.rx[m][k].path, <<n, m, gens[{n, m}]>>)
        Drain(m) == s.rx[m][k].drain \/ m \in s.drain \/ Drains(s, s.rx[m][k].path)
        R(m) == Rank(Path(m), Drain(m))
        best == CHOOSE m \in C : \A x \in C :
                    \/ Below(R(m), R(x))
                    \/ R(m) = R(x) /\ (m = current \/ (x # current /\ m <= x))
        \* The next hop holds on the path this route was built on. One that has
        \* since sent another path may wait on this node instead: were every hold
        \* in a cycle on the path it was built on, each path would extend itself.
        under == /\ current # None
                 /\ s.rx[current][k].on
                 /\ s.rx[current][k].state # "live"
                 /\ Len(was.path) > 0
                 /\ s.rx[current][k].path = SubSeq(was.path, 1, Len(was.path) - 1)
    IN IF C # {}
       THEN [on |-> TRUE, held |-> FALSE, next |-> best, path |-> Path(best), drain |-> Drain(best)]
       ELSE IF ~was.on THEN NoRoute
       ELSE IF ~under /\ ~Hopes(n, s, k) /\ ~Pending(n, s, k) THEN NoRoute
       ELSE [on |-> TRUE, held |-> TRUE, next |-> IF under THEN current ELSE None, path |-> was.path, drain |-> FALSE]

\* The publisher's own route covers everything.
OwnRoutes == [k \in Keys |-> IF k = "root"
                             THEN [on |-> TRUE, held |-> FALSE, next |-> None, path |-> <<OHop>>, drain |-> FALSE]
                             ELSE NoRoute]

\* Plan every live route before assigning children to the routes they reflect.
\* Keep the old keys as held placeholders until those assignments decide holds.
PlanKey(n, s, k, gens) ==
    LET live == SelectKey(n, s, k, gens)
    IN IF Live(live) THEN live
       ELSE IF s.rt[k].on THEN [s.rt[k] EXCEPT !.held = TRUE, !.drain = FALSE]
       ELSE NoRoute

Select(n, s, gens) ==
    IF n = Origin THEN s
    ELSE LET planned == [s EXCEPT !.rt = [k \in Keys |-> PlanKey(n, s, k, gens)]]
             sourced == [planned EXCEPT !.tx = [p \in Nodes |-> [k \in Keys |->
                 IF planned.tx[p][k].on
                 THEN [planned.tx[p][k] EXCEPT !.src = SrcKey(planned, p, k)]
                 ELSE planned.tx[p][k]]]]
         IN [sourced EXCEPT !.rt = [k \in Keys |-> SelectKey(n, sourced, k, gens)]]

-----------------------------------------------------------------------------
(* What a node knows of the claims: one view per key, the newest floor any   *)
(* route covering that key gave it, kept while it holds a covering route.   *)

Presence(s, k) == s.rt[k].on \/ (k = "a" /\ s.rt["root"].on)

\* A route this node believes: live, and it did not cross this node.
Believed(n, s, m, j) ==
    s.rx[m][j].on /\ s.rx[m][j].state = "live" /\ ~Names(s.rx[m][j].path, n)

NewView(n, s, k) ==
    IF ~Presence(s, k) THEN NoView
    ELSE LET base == IF s.view[k].floor = Absent THEN EmptyView ELSE s.view[k]
             src == {x \in Nodes \X Keys :
                        /\ x[2] = k \/ x[2] = "root"
                        /\ Believed(n, s, x[1], x[2])
                        /\ s.rx[x[1]][x[2]].rev > base.floor}
             best == CHOOSE x \in src : \A y \in src :
                        s.rx[x[1]][x[2]].rev >= s.rx[y[1]][y[2]].rev
         IN IF src = {} THEN base
            ELSE [floor |-> s.rx[best[1]][best[2]].rev,
                  items |-> Restrict(s.rx[best[1]][best[2]].items, k)]

Know(n, s, ov) ==
    IF n = Origin THEN [s EXCEPT !.view = [k \in Keys |-> [floor |-> ov.rev, items |-> Restrict(ov.items, k)]]]
    ELSE [s EXCEPT !.view = [k \in Keys |-> NewView(n, s, k)]]

\* How the reader sees each claim: from the narrowest key's view, available
\* while a live route covers it.
ShownNow(s) ==
    [c \in Claims |->
        LET v == s.view[KeyOf(c)] IN
        IF v.floor = Absent \/ v.items[c] = None THEN NotShown
        ELSE [item |-> v.items[c],
              avail |-> Live(s.rt["root"]) \/ (c \in Narrow /\ Live(s.rt["a"]))]]

-----------------------------------------------------------------------------
(* What a node sends (path.rs: `want`, `advertise`, `report`).            *)

\* What this node wants a neighbour p to hold for key k: a path, a hold, an
\* end, or to keep what it was sent while the next hop this node holds under
\* waits for those behind it.
Want(n, s, k, p) ==
    LET src == SrcKey(s, p, k)
        r == s.rt[src]
    IN IF ~r.on THEN [kind |-> "end", path |-> <<>>, drain |-> FALSE, src |-> src]
       ELSE IF ~r.held THEN [kind |-> "path", path |-> r.path, drain |-> r.drain, src |-> src]
       ELSE IF r.next = p
            THEN [kind |-> IF Pending(n, s, src) \/ Hopes(n, s, src) THEN "keep" ELSE "end",
                  path |-> <<>>, drain |-> FALSE, src |-> src]
       \* A hold names no path, so a stream holds only the path it last sent.
       ELSE IF s.tx[p][k].on /\ s.tx[p][k].path # r.path
            THEN [kind |-> "end", path |-> <<>>, drain |-> FALSE, src |-> src]
       ELSE [kind |-> "hold", path |-> <<>>, drain |-> FALSE, src |-> src]

\* `advertise` for one key on the stream to p.
Advertise1(n, s, p, k) ==
    LET t == s.tx[p][k]
        w == Want(n, s, k, p)
        v == IF s.view[k].floor = Absent THEN EmptyView ELSE s.view[k]
    IN CASE k \notin Streams(p) \/ w.kind = "keep" -> [tx |-> t, out |-> <<>>]
         [] w.kind = "hold" ->
              IF t.on /\ ~t.held THEN [tx |-> [t EXCEPT !.held = TRUE], out |-> <<HoldMsg(k)>>]
              ELSE [tx |-> t, out |-> <<>>]
         [] w.kind = "end" ->
              IF t.on THEN [tx |-> NoSent(t.src), out |-> <<EndMsg(k)>>]
              ELSE [tx |-> t, out |-> <<>>]
         [] OTHER ->
              LET offer == ~t.on \/ t.held \/ t.path # w.path \/ t.drain # w.drain
                  claims == t.rev # v.floor \/ t.items # v.items
              IN [tx |-> [on |-> TRUE, held |-> FALSE, path |-> w.path, drain |-> w.drain,
                          rev |-> v.floor, items |-> v.items, src |-> w.src],
                  out |-> (IF offer THEN <<OfferMsg(k, w.path, w.drain)>> ELSE <<>>)
                          \o (IF claims THEN <<ClaimsMsg(k, v)>> ELSE <<>>)]

Advertise(n, s, ups) ==
    LET a == [p \in Peers(n, ups) |-> [k \in Keys |-> Advertise1(n, s, p, k)]]
    IN [s |-> [s EXCEPT !.tx = [p \in Nodes |->
                  IF p \in Peers(n, ups) THEN [k \in Keys |-> a[p][k].tx] ELSE s.tx[p]]],
        out |-> [p \in Nodes |->
                  IF p \in Peers(n, ups) THEN a[p]["root"].out \o a[p]["a"].out ELSE <<>>]]

\* Messages for the keys in `ks`, in key order.
Each(ks, f(_)) ==
    LET F[i \in 0..Len(KeySeq)] ==
          IF i = 0 THEN <<>>
          ELSE F[i - 1] \o (IF KeySeq[i] \in ks THEN f(KeySeq[i]) ELSE <<>>)
    IN F[Len(KeySeq)]

\* `report` to one neighbour p: what became of the routes this node sent it
\* through `hop`, in one DEAD where it is the first word on that hop.
Report1(n, s, p, hop) ==
    LET t == s.tx[p]
        through == {k \in Keys : t[k].on /\ HasHop(t[k].path, hop)}
        w == [k \in Keys |-> Want(n, s, k, p)]
        moved == {<<k, w[k].path, w[k].drain>> : k \in {j \in through : w[j].kind = "path"}}
        holds == {k \in through : w[k].kind = "hold" /\ ~t[k].held}
        \* The neighbour this node learned the route from takes silence to mean
        \* it holds under it, so it is told outright.
        parents == {k \in holds : Child(t[k].path, p)}
        ends == {k \in through : w[k].kind = "end"}
        gone == \A k \in through : w[k].kind \in {"path", "end"}
        first == hop \notin s.told[p]
        tx1 == [k \in Keys |->
                  IF k \notin through THEN t[k]
                  ELSE CASE w[k].kind = "path" -> [t[k] EXCEPT !.path = w[k].path, !.drain = w[k].drain,
                                                              !.held = FALSE, !.src = w[k].src]
                         [] w[k].kind = "hold" -> [t[k] EXCEPT !.held = TRUE]
                         [] w[k].kind = "end" -> NoSent(t[k].src)
                         [] OTHER -> t[k]]
        out == IF first \/ (gone /\ ends # {})
               THEN <<DeadMsg(hop, moved, gone)>>
                    \o (IF gone THEN <<>>
                        ELSE Each(parents, LAMBDA k : <<HoldMsg(k)>>) \o Each(ends, LAMBDA k : <<EndMsg(k)>>))
               ELSE Each({k \in through : w[k].kind = "path"}, LAMBDA k : <<OfferMsg(k, w[k].path, w[k].drain)>>)
                    \o Each(holds, LAMBDA k : <<HoldMsg(k)>>)
                    \o Each(ends, LAMBDA k : <<EndMsg(k)>>)
    IN IF through = {} THEN [tx |-> t, told |-> s.told[p], out |-> <<>>]
       ELSE [tx |-> tx1, told |-> s.told[p] \cup {hop}, out |-> out]

Report(n, s, ups, hop) ==
    LET r == [p \in Peers(n, ups) |-> Report1(n, s, p, hop)]
    IN [s |-> [s EXCEPT !.tx = [p \in Nodes |-> IF p \in Peers(n, ups) THEN r[p].tx ELSE s.tx[p]],
                        !.told = [p \in Nodes |-> IF p \in Peers(n, ups) THEN r[p].told ELSE s.told[p]]],
        out |-> [p \in Nodes |-> IF p \in Peers(n, ups) THEN r[p].out ELSE <<>>]]

\* `report_draining`: tell each neighbour p that was sent a route through draining
\* node x, once while such a route stays sent.
ReportDrain(n, s, ups, x) ==
    LET tell == {p \in Peers(n, ups) :
                    /\ x \notin s.toldDrain[p]
                    /\ \E k \in Keys : s.tx[p][k].on /\ Names(s.tx[p][k].path, x)}
    IN [s |-> [s EXCEPT !.toldDrain = [p \in Nodes |->
                  IF p \in tell THEN s.toldDrain[p] \cup {x} ELSE s.toldDrain[p]]],
        out |-> [p \in Nodes |-> IF p \in tell THEN <<DrainMsg(x)>> ELSE <<>>]]

\* `collect`: forget each dead hop no route names once its timer has fired,
\* and each told hop no sent route names. Doubt lasts until Lift fires.
Collect(s) ==
    LET heard == {s.rx[x[1]][x[2]].path : x \in {y \in Nodes \X Keys : s.rx[y[1]][y[2]].on}}
    IN [s EXCEPT
          !.dead = {h \in s.dead : h \in s.expiry \/ \E p \in heard : HasHop(p, h)},
          !.draining = {x \in s.draining : x \in s.doubt \/ \E p \in heard : Names(p, x)},
          !.told = [m \in Nodes |-> {h \in s.told[m] :
                       \E k \in Keys : s.tx[m][k].on /\ HasHop(s.tx[m][k].path, h)}],
          !.toldDrain = [m \in Nodes |-> {x \in s.toldDrain[m] :
                       \E k \in Keys : s.tx[m][k].on /\ Names(s.tx[m][k].path, x)}]]

-----------------------------------------------------------------------------
(* `update`: select, learn, report a dead hop if any, advertise, collect,   *)
(* show. Returns the node's new state, its messages per neighbour, and its  *)
(* reader's events.                                                         *)

NoReport == [on |-> FALSE, hop |-> OHop, node |-> None]
DeadReport(hop) == [NoReport EXCEPT !.on = TRUE, !.hop = hop]
DrainReport(x) == [NoReport EXCEPT !.node = x]

\* The reader's events: changes to `ended` and the counters. A claim leaving
\* while the node still holds a covering route is an END, else it went offline.
\* `quiet` marks a step that only changed a path or lifted a doubt: in one, a
\* claim the reader sees changes only to what a newer floor says, or goes
\* offline after it was held unavailable.
Events(s0, s, old, now, ov, eb, quiet) ==
    LET present(c) == Presence(s, KeyOf(c))
        isEnd(c) == old[c].item # None /\ now[c].item = None /\ present(c)
        isStart(c) == old[c].item = None /\ now[c].item # None
        isUpdate(c) == old[c].item # None /\ now[c].item # None /\ old[c].item # now[c].item
        \* Forgetting a claim clears what the reader saw end.
        eb1 == [c \in Claims |-> IF present(c) THEN eb[c] ELSE None]
        newer(c) == s.view[KeyOf(c)].floor > s0.view[KeyOf(c)].floor
        before(c) == Presence(s0, KeyOf(c))
        \* Letting go of a claim it held unavailable ends a hold, not a path.
        release(c) == ~old[c].avail /\ ~present(c)
    IN [ended |-> [c \in Claims |-> IF isEnd(c) THEN old[c].item ELSE eb1[c]],
        revivals |-> Cardinality({c \in Claims : isStart(c) /\ eb[c] = now[c].item}),
        falseEnds |-> Cardinality({c \in Claims : isEnd(c) /\ ov.items[c] = old[c].item}),
        regressions |-> Cardinality({c \in Claims : isUpdate(c) /\ now[c].item < old[c].item}),
        unstable |-> IF ~quiet THEN 0
                     ELSE Cardinality({c \in Claims : old[c].item # now[c].item /\ before(c) /\ ~newer(c) /\ ~release(c)})]

Update(n, s0, ov, ups, gens, eb, rep, quiet) ==
    LET s1 == Select(n, s0, gens)
        s2 == Know(n, s1, ov)
        r == IF rep.on THEN Report(n, s2, ups, rep.hop)
             ELSE IF rep.node # None THEN ReportDrain(n, s2, ups, rep.node)
             ELSE [s |-> s2, out |-> [p \in Nodes |-> <<>>]]
        a == Advertise(n, r.s, ups)
        s3 == Collect(a.s)
        now == IF n = Origin THEN [c \in Claims |-> NotShown] ELSE ShownNow(s3)
        ev == Events(s0, s3, s0.shown, now, ov, eb, quiet)
    IN [s |-> [s3 EXCEPT !.shown = now],
        out |-> [p \in Nodes |-> r.out[p] \o a.out[p]],
        ev |-> ev,
        dead |-> a.s.dead]

\* The publisher ignores every message it hears for its own route, so none is
\* queued toward it. Dropping them changes no behaviour; `Budget` still counts them.
Heeded(d, m) == d[2] # Origin

\* The sum of f(x) for each x in the finite set S.
\* FoldSet runs natively through the Community Modules' Java override; an
\* instance keeps their `Restrict` from clashing with this module's.
LOCAL Ext == INSTANCE FiniteSetsExt
SumOver(S, f(_)) == Ext!FoldSet(LAMBDA x, acc : f(x) + acc, 0, S)

\* Commit the updates `rs` (a function from nodes to `Update` results), appending
\* each node's messages to `q0`. Nodes in one step send on disjoint queues.
Commit(rs, q0) ==
    LET ns == DOMAIN rs
        Sum(f(_)) == SumOver(ns, f)
    IN /\ st' = [n \in Nodes |-> IF n \in ns THEN rs[n].s ELSE st[n]]
       /\ q' = [d \in Dirs |-> IF d[1] \in ns THEN q0[d] \o SelectSeq(rs[d[1]].out[d[2]], LAMBDA m : Heeded(d, m))
                                 ELSE q0[d]]
       /\ ended' = [n \in Nodes |-> IF n \in ns THEN rs[n].ev.ended ELSE ended[n]]
       /\ revivals' = revivals + Sum(LAMBDA n : rs[n].ev.revivals)
       /\ falseEnds' = falseEnds + Sum(LAMBDA n : rs[n].ev.falseEnds)
       /\ regressions' = regressions + Sum(LAMBDA n : rs[n].ev.regressions)
       /\ unstable' = unstable + Sum(LAMBDA n : rs[n].ev.unstable)
       /\ learnt' = [n \in Nodes |-> IF n \in ns THEN learnt[n] \cup rs[n].dead ELSE learnt[n]]

Quiet == \A d \in Dirs : q[d] = <<>>
NoDoubt == \A n \in Nodes : st[n].wait = {} /\ st[n].expiry = {}

\* The messages the updates `rs` send, counted before `Heeded` drops any.
Sent(rs) == SumOver(DOMAIN rs, LAMBDA n : SumOver(Nodes, LAMBDA m : Len(rs[n].out[m])))

\* A burst runs from one quiet point to the next. An external change (a claim
\* change, a session opening or closing) made while quiet starts a fresh one;
\* every message sent in it counts against the changes made in it.
External(sent) ==
    /\ burst' = (IF Quiet THEN 0 ELSE burst) + sent
    /\ churn' = (IF Quiet THEN 0 ELSE churn) + 1

Internal(sent) ==
    /\ burst' = burst + sent
    /\ UNCHANGED churn

-----------------------------------------------------------------------------
(* Initial state: every session open, the publisher's route sent and no     *)
(* claims yet.                                                              *)

InitNode == [
    rx |-> [m \in Nodes |-> [k \in Keys |-> NoHeard]],
    rt |-> [k \in Keys |-> NoRoute],
    view |-> [k \in Keys |-> NoView],
    dead |-> {},
    doubt |-> {},
    wait |-> {},
    expiry |-> {}, \* Pending timers belong to individual dead hops.
    drain |-> {},  \* the neighbours that sent GOAWAY on the session open to them
    draining |-> {}, \* nodes known to have sent GOAWAY, while named or doubted
    tx |-> [m \in Nodes |-> [k \in Keys |-> NoSent(k)]],
    told |-> [m \in Nodes |-> {}],
    toldDrain |-> [m \in Nodes |-> {}],
    shown |-> [c \in Claims |-> NotShown]
]

OV0 == [rev |-> 0, items |-> NoItems]
Gen0 == [l \in Links |-> 0]
Start == Update(Origin, [InitNode EXCEPT !.rt = OwnRoutes], OV0, Links, Gen0,
                [c \in Claims |-> None], NoReport, FALSE)

Init ==
    /\ up = Links
    /\ gen = Gen0
    /\ q = [d \in Dirs |-> IF d[1] = Origin THEN Start.out[d[2]] ELSE <<>>]
    /\ rev = 0
    /\ own = NoItems
    /\ hist = <<NoItems>>
    /\ st = [n \in Nodes |-> IF n = Origin THEN Start.s ELSE InitNode]
    /\ ended = [n \in Nodes |-> [c \in Claims |-> None]]
    /\ revivals = 0
    /\ falseEnds = 0
    /\ regressions = 0
    /\ unstable = 0
    /\ learnt = [n \in Nodes |-> {}]
    /\ flaps = 0
    /\ burst = 0
    /\ churn = 1

OV == [rev |-> rev, items |-> own]

-----------------------------------------------------------------------------
(* The publisher changes a claim.                                           *)

ChangeClaim(c, item) ==
    LET own1 == [own EXCEPT ![c] = item]
        ov1 == [rev |-> rev + 1, items |-> own1]
        rs == [n \in {Origin} |-> Update(Origin, st[Origin], ov1, up, gen, ended[Origin], NoReport, FALSE)]
    IN /\ rev' = rev + 1
       /\ own' = own1
       /\ hist' = Append(hist, own1)
       /\ Commit(rs, q)
       /\ External(Sent(rs))
       /\ UNCHANGED <<up, gen, flaps>>

Announce(c) == rev < MaxRev /\ ChangeClaim(c, rev + 1)
Unannounce(c) == rev < MaxRev /\ own[c] # None /\ ChangeClaim(c, None)

-----------------------------------------------------------------------------
(* A message arrives (path.rs: `recv`, `fail`).                            *)

\* `fail`: the session `hop` names closed. `from` reported it with what became
\* of its own routes through it, or this node saw it close (`from` is None).
\* It casts doubt on the node at the session's far end, `hop[2]`.
Fail(n, s, hop, from, moved, gone) ==
    LET Moved(k) == {mv \in moved : mv[1] = k}
        rx1 == [p \in Nodes |-> [k \in Keys |->
                  LET h == s.rx[p][k] IN
                  IF ~h.on \/ ~HasHop(h.path, hop) THEN h
                  ELSE IF p = from /\ Moved(k) # {}
                       THEN LET mv == CHOOSE mv \in Moved(k) : TRUE
                                path == mv[2]
                            IN [h EXCEPT !.path = path, !.drain = mv[3],
                                         !.state = IF DeadIn(path, s.dead) THEN "dead" ELSE "live"]
                  ELSE IF p = from
                       THEN IF gone THEN NoHeard
                            \* A neighbour that holds under this node answers once those behind it have.
                            ELSE IF ~Child(h.path, n) THEN [h EXCEPT !.state = "held"] ELSE h
                  ELSE IF h.state = "live" THEN [h EXCEPT !.state = "dead"] ELSE h]]
    IN [s EXCEPT !.rx = rx1, !.dead = @ \cup {hop},
                 !.expiry = IF hop \in s.dead THEN @ ELSE @ \cup {hop},
                 !.doubt = @ \cup ({hop[2]} \ {n}), !.wait = @ \cup ({hop[2]} \ {n})]

\* `drains`: x sent GOAWAY. The first word of it casts doubt on x, so no relay
\* takes a stale offer through it until the timer fires.
MarkDrain(n, s, x) ==
    IF x \in s.draining \/ x = n THEN s
    ELSE [s EXCEPT !.draining = @ \cup {x}, !.doubt = @ \cup {x}, !.wait = @ \cup {x}]

\* A DRAIN counts only for a node on a path `from` offered.
Credited(s, from, x) == \E k \in Keys : s.rx[from][k].on /\ Names(s.rx[from][k].path, x)

Recv(n, s, from, msg) ==
    LET k == msg.k
        h == s.rx[from][k]
    IN CASE msg.t = "offer" ->
              [s EXCEPT !.rx[from][k] =
                  [on |-> TRUE, path |-> msg.path, drain |-> msg.drain,
                   state |-> IF DeadIn(msg.path, s.dead) THEN "dead" ELSE "live",
                   rev |-> IF h.on THEN h.rev ELSE None,
                   items |-> IF h.on THEN h.items ELSE NoItems]]
         [] msg.t = "hold" -> IF h.on THEN [s EXCEPT !.rx[from][k].state = "held"] ELSE s
         [] msg.t = "end" -> [s EXCEPT !.rx[from][k] = NoHeard]
         [] msg.t = "claims" ->
              IF h.on THEN [s EXCEPT !.rx[from][k].rev = msg.rev, !.rx[from][k].items = msg.items] ELSE s
         [] msg.t = "drain" -> IF Credited(s, from, msg.node) THEN MarkDrain(n, s, msg.node) ELSE s
         [] OTHER -> Fail(n, s, msg.hop, from, msg.moved, msg.gone)

Deliver(d) ==
    LET m == d[1]
        n == d[2]
        msg == Head(q[d])
        q0 == [q EXCEPT ![d] = Tail(@)]
        rep == CASE msg.t = "dead" -> DeadReport(msg.hop)
                 [] msg.t = "drain" /\ msg.node # n /\ Credited(st[n], m, msg.node) -> DrainReport(msg.node)
                 [] OTHER -> NoReport
        \* Only the route's path, or how it ranks, changed.
        quiet == \/ msg.t = "offer" /\ st[n].rx[m][msg.k].on
                 \/ msg.t = "drain"
        rs == [x \in {n} |-> Update(n, Recv(n, st[n], m, msg), OV, up, gen, ended[n], rep, quiet)]
    IN /\ q[d] # <<>>
       /\ Commit(rs, q0)
       /\ Internal(Sent(rs))
       /\ UNCHANGED <<up, gen, rev, own, hist, flaps>>

\* The timer node n started on x fires: the doubt on x lifts, and the dead hops
\* whose session ended at x may be forgotten. With `Patient`, only once no DEAD
\* about such a hop, and no DRAIN of x, is in flight: a timer long enough for
\* the news to cross the mesh.
Lift(n, x) ==
    LET rs == [y \in {n} |-> Update(n, [st[n] EXCEPT !.doubt = @ \ {x}, !.wait = @ \ {x},
                                     !.expiry = {h \in @ : h[2] # x}],
                                     OV, up, gen, ended[n], NoReport, TRUE)]
    IN /\ x \in st[n].wait
       /\ Patient => \A d \in Dirs : \A i \in DOMAIN q[d] :
                        ~(q[d][i].t = "dead" /\ q[d][i].hop[2] = x) /\ ~(q[d][i].t = "drain" /\ q[d][i].node = x)
       /\ Commit(rs, q)
       /\ Internal(Sent(rs))
       /\ UNCHANGED <<up, gen, rev, own, hist, flaps>>

\* A hop's own timer can fire before a later failure's timer on the same far
\* node. A new DEAD cannot extend every old hop's retention indefinitely.
Expire(n, hop) ==
    LET rs == [y \in {n} |-> Update(n, [st[n] EXCEPT !.expiry = @ \ {hop}],
                                     OV, up, gen, ended[n], NoReport, TRUE)]
    IN /\ hop \in st[n].expiry
       /\ Patient => \A d \in Dirs : \A i \in DOMAIN q[d] : ~(q[d][i].t = "dead" /\ q[d][i].hop = hop)
       /\ Commit(rs, q)
       /\ Internal(Sent(rs))
       /\ UNCHANGED <<up, gen, rev, own, hist, flaps>>

-----------------------------------------------------------------------------
(* Sessions open, drain, and close.                                         *)

\* `goaway`: n hears GOAWAY from m. Every path through m ranks below every path
\* that avoids it and stays usable; n's own offers through it say so. n doubts
\* m, so it moves at once only onto a path that avoids m, and tells each
\* neighbour it sent a route through m.
GoAway(n, m) ==
    LET rs == [x \in {n} |-> Update(n, MarkDrain(n, [st[n] EXCEPT !.drain = @ \cup {m}], m),
                                     OV, up, gen, ended[n], DrainReport(m), TRUE)]
    IN /\ {n, m} \in up
       /\ m \notin st[n].drain
       /\ Commit(rs, q)
       /\ External(Sent(rs))
       /\ UNCHANGED <<up, gen, rev, own, hist, flaps>>

\* A closed session: everything learned or sent on it goes.
Closed(s, y) ==
    [s EXCEPT !.rx[y] = [k \in Keys |-> NoHeard],
              !.tx[y] = [k \in Keys |-> NoSent(k)],
              !.told[y] = {},
              !.toldDrain[y] = {},
              !.drain = @ \ {y}]

Connect(l) ==
    LET up1 == up \cup {l}
        gen1 == [gen EXCEPT ![l] = @ + 1]
        rs == [x \in l |->
                 LET y == CHOOSE y \in l : y # x
                 IN Update(x, Closed(st[x], y), OV, up1, gen1, ended[x], NoReport, FALSE)]
    IN /\ l \in Flaky
       /\ l \notin up
       /\ flaps < MaxFlaps
       /\ up' = up1
       /\ gen' = gen1
       /\ flaps' = flaps + 1
       /\ Commit(rs, q)
       /\ External(Sent(rs))
       /\ UNCHANGED <<rev, own, hist>>

\* `disconnect`: each end names the hop it learned routes on there.
Disconnect(l) ==
    LET up1 == up \ {l}
        q0 == [d \in Dirs |-> IF {d[1], d[2]} = l THEN <<>> ELSE q[d]]
        rs == [x \in l |->
                 LET y == CHOOSE y \in l : y # x
                     hop == <<x, y, gen[l]>>
                     s == Closed(st[x], y)
                 IN IF x = Origin
                    THEN Update(x, s, OV, up1, gen, ended[x], NoReport, FALSE)
                    ELSE Update(x, Fail(x, s, hop, None, {}, FALSE), OV, up1, gen, ended[x],
                                DeadReport(hop), FALSE)]
    IN /\ l \in Flaky
       /\ l \in up
       /\ flaps < MaxFlaps
       /\ up' = up1
       /\ flaps' = flaps + 1
       /\ Commit(rs, q0)
       /\ External(Sent(rs))
       /\ UNCHANGED <<gen, rev, own, hist>>

\* Nothing is in flight and no doubt is left: the run may stop here.
Done ==
    /\ Quiet
    /\ NoDoubt
    /\ UNCHANGED vars

Next ==
    \/ \E c \in Claims : Announce(c) \/ Unannounce(c)
    \/ \E d \in Dirs : Deliver(d)
    \/ \E n \in Relays, x \in Nodes : Lift(n, x)
    \/ \E n \in Relays : \E hop \in st[n].expiry : Expire(n, hop)
    \/ \E l \in Links : Connect(l) \/ Disconnect(l)
    \/ \E n \in Relays, m \in Leaving : GoAway(n, m)
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

\* Every floor a node keeps or was offered is the publisher's claims at that
\* revision, within its key.
Truthful ==
    \A n \in Relays :
        /\ \A k \in Keys : st[n].view[k].floor >= 0 =>
              st[n].view[k].items = Restrict(hist[st[n].view[k].floor + 1], k)
        /\ \A m \in Nodes, k \in Keys :
              st[n].rx[m][k].on /\ st[n].rx[m][k].rev >= 0 =>
                  st[n].rx[m][k].items = Restrict(hist[st[n].rx[m][k].rev + 1], k)

\* A reader never sees a broadcast start again after it saw it end.
NoRevival == revivals = 0

\* A reader never sees a broadcast end while the publisher still has it.
NoFalseEnd == falseEnds = 0

\* A reader never sees an older version replace a newer one.
NoRegression == regressions = 0

\* A change of path, or a lifted doubt, alone starts, ends, and changes no claim
\* a reader sees: only a newer floor does. Whether it is available may change.
Stable == unstable = 0

\* Whether node n shows claim c (Seen): exactly while it holds a route that
\* covers c, as the newest floor a covering route gave it says.
Seen ==
    \A n \in Relays, c \in Claims :
        LET v == st[n].view[KeyOf(c)] IN
        /\ st[n].shown[c].item # None <=> (Presence(st[n], KeyOf(c)) /\ v.floor >= 0 /\ v.items[c] # None)
        /\ st[n].shown[c].item # None => st[n].shown[c].item = v.items[c]
        \* No believed covering route has a newer floor than the view.
        /\ Presence(st[n], KeyOf(c)) =>
              \A m \in Nodes, j \in Keys :
                  (j = KeyOf(c) \/ j = "root") /\ Believed(n, st[n], m, j) =>
                      st[n].rx[m][j].rev <= v.floor

\* The loop-free paths from the publisher over the configured links, as the
\* nodes each crosses in order, and what each costs its last node.
RECURSIVE Extend(_, _)
Last(p) == p[Len(p)]
Extend(P, k) ==
    IF k = 0 THEN P
    ELSE Extend(P \cup UNION {{Append(p, n) : n \in {m \in Relays : {Last(p), m} \in Links}} : p \in P}, k - 1)
Paths ==
    {p \in Extend({<<Origin>>}, Cardinality(Relays)) : \A i, j \in DOMAIN p : i # j => p[i] # p[j]}

NodesOf(path) == [i \in DOMAIN path |-> path[i][1]]
IsPath(from, path) ==
    /\ Len(path) >= 1
    /\ path[1] = OHop
    /\ NodesOf(path) \in Paths
    /\ Last(NodesOf(path)) = from
    /\ \A i \in DOMAIN path : i > 1 => path[i][2] = path[i - 1][1]

\* Every route held or in flight names a loop-free path from the publisher to
\* its sender.
SimplePaths ==
    /\ \A n, m \in Nodes, k \in Keys : st[n].rx[m][k].on => IsPath(m, st[n].rx[m][k].path)
    /\ \A d \in Dirs : \A i \in DOMAIN q[d] :
          /\ q[d][i].t = "offer" => IsPath(d[1], q[d][i].path)
          /\ q[d][i].t = "dead" => \A mv \in q[d][i].moved : IsPath(d[1], mv[2])

\* A hop whose session has closed: the link is down or opened again since.
Gone(h) == h # OHop /\ ({h[1], h[2]} \notin up \/ gen[{h[1], h[2]}] # h[3])
Stale(path) == \E i \in DOMAIN path : Gone(path[i])

\* Where a request for a claim goes. A node starts it on the narrowest live
\* route covering the claim; each node forwards it on the route it offered the
\* node it came from under that key, and never along a route whose path names
\* that node. The result is "origin", "stuck", "refused", or "loop".
RECURSIVE Fwd(_, _, _, _)
Fwd(cur, k, prev, i) ==
    IF cur = Origin THEN "origin"
    ELSE IF i = 0 THEN "loop"
    ELSE LET src == IF prev = None THEN k ELSE SrcKey(st[cur], prev, k)
             r == st[cur].rt[src]
         IN IF ~Live(r) THEN "stuck"
            ELSE IF prev # None /\ Names(r.path, prev) THEN "refused"
            ELSE Fwd(r.next, src, cur, i - 1)

Request(n, c) ==
    Fwd(n, IF c \in Narrow /\ Live(st[n].rt["a"]) THEN "a" ELSE "root", None, Cardinality(Nodes))

\* The nodes a request from n for c crosses before it reaches the publisher,
\* stops, or has crossed every node.
RECURSIVE Walk(_, _, _, _)
Walk(cur, k, prev, i) ==
    IF cur = Origin \/ i = 0 THEN <<>>
    ELSE LET src == IF prev = None THEN k ELSE SrcKey(st[cur], prev, k)
             r == st[cur].rt[src]
         IN IF ~Live(r) \/ (prev # None /\ Names(r.path, prev)) THEN <<>>
            ELSE <<[node |-> cur, path |-> r.path]>> \o Walk(r.next, src, cur, i - 1)

\* A request that loops crosses a node whose route is stale: it names a
\* session that has closed, whose news has yet to reach that node.
LoopsAreStale ==
    \A n \in Relays, c \in Claims :
        (Live(st[n].rt["root"]) \/ Live(st[n].rt["a"])) /\ Request(n, c) = "loop" =>
                LET w == Walk(n, IF c \in Narrow /\ Live(st[n].rt["a"]) THEN "a" ELSE "root", None, Cardinality(Nodes))
                IN \E i \in DOMAIN w : Stale(w[i].path)

\* A request never loops. It does while news is in flight: `Loop.cfg` pins it.
RequestsLoopFree == \A n \in Relays, c \in Claims : Request(n, c) # "loop"

\* A request that loops goes round relays whose routes each begin on a closed
\* session of the publisher: it left every one of them, and their news is in
\* flight. Moving onto a route that begins at a doubted node allows that loop;
\* doubt about a relay still keeps every other one out.
LoopsLostThePublisher ==
    \A n \in Relays, c \in Claims :
        Request(n, c) = "loop" =>
            LET w == Walk(n, IF c \in Narrow /\ Live(st[n].rt["a"]) THEN "a" ELSE "root", None, Cardinality(Nodes))
                \* The positions inside a stretch of the walk that comes back to its start.
                round == {i \in DOMAIN w : \E j, k \in DOMAIN w : j <= i /\ i < k /\ w[j].node = w[k].node}
            IN \A i \in round : Len(w[i].path) >= 2 /\ Gone(w[i].path[2])

\* A route a relay knows over sessions that are still open, crossing no relay
\* it doubts.
Fresh(n, m, k) ==
    /\ Usable(n, st[n], m, k)
    /\ ~Stale(st[n].rx[m][k].path)
    /\ ~RelayDoubted(st[n], st[n].rx[m][k].path)

\* Doubt about the publisher alone never leaves a relay without a route: one
\* that knows a fresh route uses some route, so a request stops only at a relay
\* that knows no way on. Which route it uses may still be stale while the news
\* that ends it is in flight, which `StaleIsChased` bounds.
Routed == \A n \in Relays, k \in Keys : (\E m \in Nodes : Fresh(n, m, k)) => Live(st[n].rt[k])

\* No node may use a route through a hop it once learned had died: after a
\* dead hop is forgotten, no stale route returns. It holds only for one failure
\* and a patient timer; `StaleIsChased` bounds the rest.
NoStaleReturn ==
    \A n \in Relays, m \in Nodes, k \in Keys :
        Usable(n, st[n], m, k) => ~DeadIn(st[n].rx[m][k].path, learnt[n])

\* Whether the news that ends a stale route node n heard from m under key k is
\* on its way: queued from m to n, or m's own route is stale and its news is
\* on its way to m.
RECURSIVE Chased(_, _, _, _)
Chased(n, m, k, i) ==
    \/ q[<<m, n>>] # <<>>
    \/ /\ i > 0
       /\ m # Origin
       /\ LET src == SrcKey(st[m], n, k)
              r == st[m].rt[src]
          IN /\ Live(r)
             /\ Stale(r.path)
             /\ Chased(m, r.next, src, i - 1)

\* A stale route a node may use always has the news that ends it on its way,
\* so it lasts no longer than that news takes to arrive.
StaleIsChased ==
    \A n \in Relays, m \in Nodes, k \in Keys :
        Usable(n, st[n], m, k) /\ Stale(st[n].rx[m][k].path) => Chased(n, m, k, Cardinality(Nodes))

\* The nodes a path over open sessions from the publisher reaches, through
\* nodes whose interest covers claim c.
RECURSIVE Grow(_, _, _)
Grow(R, c, i) == IF i = 0 THEN R
                 ELSE Grow(R \cup {n \in Relays : Covers(Ask(n), c) /\ \E m \in R : {m, n} \in up}, c, i - 1)
Carried(c) == Grow({Origin}, c, Cardinality(Nodes))

\* Once nothing is in flight and no doubt is left: each relay shows exactly the
\* publisher's claims that a path carries to it, available; a request for each
\* reaches the publisher; no relay holds a route or keeps a stale one; and
\* every dead hop is forgotten.
Settled ==
    /\ \A n \in Relays :
          /\ \A c \in Claims :
                st[n].shown[c] = IF n \in Carried(c) /\ own[c] # None
                                 THEN [item |-> own[c], avail |-> TRUE] ELSE NotShown
          /\ \A k \in Keys : ~st[n].rt[k].held
          /\ \A m \in Nodes, k \in Keys :
                st[n].rx[m][k].on => st[n].rx[m][k].state = "live" /\ ~Stale(st[n].rx[m][k].path)
          /\ st[n].dead = {}
    /\ \A n \in Relays, c \in Claims : st[n].shown[c].avail => Request(n, c) = "origin"

QuietIsSettled == Quiet /\ NoDoubt => Settled

\* What one external change may cost: 4 messages each way over each configured
\* session per key and per claim.
PerChange == 4 * Cardinality(Dirs) * (Cardinality(Keys) + Cardinality(Claims))

\* The messages sent since the last quiet point stay within the budget of the
\* external changes made since then. `burst` is part of the state, so a cycle of
\* deliveries that never goes quiet would grow it past any budget.
Budget == burst <= PerChange * churn

=============================================================================
