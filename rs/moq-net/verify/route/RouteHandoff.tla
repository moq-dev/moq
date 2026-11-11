---------------------------- MODULE RouteHandoff ----------------------------
(***************************************************************************)
(* The lite-07 route layer for one origin identity, as `route::Router`     *)
(* (rs/moq-net/src/route/mod.rs) runs it and `route::tests::Sim` drives it. *)
(*                                                                         *)
(* Each node is one Router. Every router entry point (`route`, `batch`,    *)
(* `request`, `echo`, `collect`, `connect`/`peer_interest`, `disconnect`,  *)
(* `announce`/`unannounce`) is one atomic step that ends in `update`, as   *)
(* in the code. Messages travel as the Sim queues them: ROUTE, batches,    *)
(* ROUTE_REQUEST, and echoed ROUTE_ENDs on one FIFO per direction, so an  *)
(* echo arrives behind every route its sender sent before it. A session    *)
(* opens and closes atomically, dropping what was in flight.               *)
(*                                                                         *)
(* Three switches compare ways for a node to forget a feasibility distance *)
(* safely. `Echo` is "once" for an echo sent as soon as the ROUTE_END is   *)
(* applied, "transitive" for one a node that was routing through the       *)
(* sender defers until its own ROUTE_ENDs are echoed, or "none".           *)
(* `EchoLane` lets an echo overtake its sender's other messages, as it can *)
(* on a stream of its own. `Hops` makes each route name the nodes it       *)
(* crossed, and a node never uses a route that names it.                   *)
(*                                                                         *)
(* Scope: every node watches the root, links cost 1 with equal priority,   *)
(* no scoped grants, no GOAWAY, and the origin keeps its identity. Claims  *)
(* are exact paths, and a claim's attributes are the revision that last    *)
(* set them, standing in for a fresh Instance per announce.                *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Nodes,        \* router keys; the Sim's `Node` index, ordered as in `selected`
    Origin,       \* the node that declares the identity
    Leaves,       \* nodes that send only identities they serve (`Sim::leaf`)
    Links,        \* sessions, as 2-element sets of nodes
    Cost,         \* Cost[l]: the cost of link l in Links, strictly positive
    Claims,       \* claim paths
    MaxRev,       \* the origin's claim changes
    MaxFlaps,     \* session opens and closes
    Echo,         \* how a ROUTE_END is echoed: "once" at once, "transitive" only
                  \* once the echoer's own ROUTE_ENDs are, or "none" for no echo
    EchoLane,     \* TRUE: echoes travel unordered against the sender's other messages
    Hops,         \* TRUE: a route names the nodes it crossed, and a node refuses one naming itself
    MaxCollects   \* `collect` calls

Relays == Nodes \ {Origin}
\* Every link costs 1: a config binds `Cost <- Unit`.
Unit == [l \in Links |-> 1]
Transitive == Echo = "transitive"

\* A claim's attributes: the revision that last set it, or None when absent.
None == -1
\* A view's floor: a revision, None for no floor, or Absent for no view at all.
Absent == -2

NoItems == [c \in Claims |-> None]
EmptyView == [floor |-> None, items |-> NoItems]
NoView == [floor |-> Absent, items |-> NoItems]

\* `Reach` as sent on a stream: nothing, `Finite(Advert)`, or `Held`.
NoReach == [kind |-> "none", seq |-> 0, met |-> 0, hops |-> {}]
HeldReach == [kind |-> "held", seq |-> 0, met |-> 0, hops |-> {}]
Finite(seq, met, hops) == [kind |-> "fin", seq |-> seq, met |-> met, hops |-> hops]

\* `rx` entry, `fd`, `heard`, and `pending` when absent.
NoEntry == [on |-> FALSE, seq |-> 0, met |-> 0, held |-> FALSE, hops |-> {}, mine |-> FALSE]
NoFd == [on |-> FALSE, seq |-> 0, met |-> 0]
NoHeard == [on |-> FALSE, raw |-> EmptyView, kept |-> NoView, ended |-> FALSE]
NoPending == [on |-> FALSE, seq |-> 0, from |-> {}, asked |-> None]
NotShown == [item |-> None, avail |-> FALSE]
\* `owed` entry: whether a neighbor's ROUTE_END awaits its echo, and the
\* neighbors whose own echoes it waits on.
NotOwed == [on |-> FALSE, wait |-> {}]
\* Owed, and not yet waiting on anyone: `Update` fills that in once the node
\* selects a route again or ends its own.
Undecided == [on |-> TRUE, wait |-> {None}]

\* Messages: one record shape so TLC can compare them.
RouteMsg(reach) == [type |-> "route", reach |-> reach, view |-> NoView, seq |-> 0]
BatchMsg(view) == [type |-> "batch", reach |-> NoReach, view |-> view, seq |-> 0]
RequestMsg(seq) == [type |-> "request", reach |-> NoReach, view |-> NoView, seq |-> seq]
EchoMsg == [type |-> "echo", reach |-> NoReach, view |-> NoView, seq |-> 0]

VARIABLES
    up,         \* open sessions (`Sim::up`)
    q,          \* q[<<m, n>>]: ROUTE, batches, and ROUTE_REQUEST from m to n, in order
    rev,        \* the origin's claim revision (`Origin::view.floors[""]`)
    own,        \* the origin's claims (`Origin::view.items`)
    seqno,      \* the origin's route seqno (`Origin::seqno`)
    hist,       \* hist[r + 1]: the origin's claims at revision r
    st,         \* st[n]: node n's Router state for the identity
    ended,      \* ended[n][c]: the item n's reader last saw END for, until n forgets c
    revivals,   \* reader STARTs of an item the reader saw END for (`revivals`)
    falseEnds,  \* reader ENDs while the origin still has that item (`false_ends`)
    regressions,\* reader UPDATEs to an older item
    flaps,
    collects

vars == <<up, q, rev, own, seqno, hist, st, ended, revivals, falseEnds,
          regressions, flaps, collects>>

\* The origin's view and seqno.
OV == [rev |-> rev, items |-> own, seq |-> seqno]

Dirs == {<<m, n>> \in Nodes \X Nodes : {m, n} \in Links}
Peers(n, ups) == {m \in Nodes : {n, m} \in ups}

\* `Link::proxy`: a leaf sends no identity it learned.
Serves(n) == n = Origin \/ n \notin Leaves

-----------------------------------------------------------------------------
(* Route selection on one node's state `s` (mod.rs: `feasible`, `selected`, *)
(* `infeasible`, `recoverable`, `live`, `certifiers`). Every link costs 1.  *)

Feasible(s, e) ==
    ~s.fd.on \/ e.seq > s.fd.seq \/ (e.seq = s.fd.seq /\ e.met < s.fd.met)

\* A route naming this node (`mine`) came from it: it is kept until it ends,
\* and is never selected, counted as infeasible, or a certifier.
UsableSet(s) == {m \in Nodes : s.rx[m].on /\ ~s.rx[m].mine /\ ~s.rx[m].held /\ Feasible(s, s.rx[m])}
InfeasibleSet(s) == {m \in Nodes : s.rx[m].on /\ ~s.rx[m].mine /\ ~s.rx[m].held /\ ~Feasible(s, s.rx[m])}
Certifiers(s) == {m \in Nodes : s.rx[m].on /\ ~s.rx[m].mine /\ Feasible(s, s.rx[m])}
Recoverable(s) == \E m \in Certifiers(s) : s.rx[m].held

\* The lowest metric, then the lowest key: `selected`'s rank with equal priority.
Best(n, s, S) ==
    CHOOSE m \in S : \A k \in S :
        LET mm == s.rx[m].met + Cost[{n, m}]
            mk == s.rx[k].met + Cost[{n, k}]
        IN mm < mk \/ (mm = mk /\ m <= k)

NoSel == [on |-> FALSE, next |-> None, seq |-> 0, met |-> 0, hops |-> {}]
Selected(n, s, ov) ==
    IF n = Origin THEN [on |-> TRUE, next |-> None, seq |-> ov.seq, met |-> 0, hops |-> {}]
    ELSE IF UsableSet(s) = {} THEN NoSel
    ELSE LET m == Best(n, s, UsableSet(s))
         IN [on |-> TRUE, next |-> m, seq |-> s.rx[m].seq, met |-> s.rx[m].met + Cost[{n, m}],
             hops |-> s.rx[m].hops]

\* Whether node n's route comes from neighbor m: n selected m's route, or
\* selects nothing and holds on m's held one.
Through(n, s, m, sel) ==
    IF sel.on THEN sel.next = m
    ELSE n # Origin /\ m \in Certifiers(s) /\ s.rx[m].held

Live(n, s) ==
    n = Origin \/ UsableSet(s) # {} \/ InfeasibleSet(s) # {} \/ Recoverable(s)

\* `Heard::view`: what it certified before its route ended, else the raw view.
HeardView(h) == IF h.kept.floor # Absent THEN h.kept ELSE h.raw

\* `certified(id, held)` at the root interest: `ok` is FALSE where the code
\* returns None (no route, or a certifier has not answered), and `root` says
\* whether any counted certifier's view has a floor over the root.
Certified(s, held) ==
    [ok |-> (\E m \in Nodes : s.rx[m].on) /\ \A m \in Certifiers(s) : s.heard[m].on,
     root |-> \E m \in Certifiers(s) :
                (held \/ ~s.rx[m].held) /\ HeardView(s.heard[m]).floor >= 0]

\* The view the node shows and forwards (`adopted`). Only certified claims
\* exist at the root interest (invariant AllCertified), so `loose` adds nothing.
Adopted(n, s, ov) ==
    IF n = Origin THEN [floor |-> ov.rev, items |-> ov.items]
    ELSE IF ~Live(n, s) \/ s.view.floor = Absent THEN EmptyView
    ELSE s.view

\* `View::floor` of what `adopted_floors` gives: the merged view even while not live.
FloorOf(n, s, ov) == IF n = Origin THEN ov.rev ELSE IF s.view.floor = Absent THEN None ELSE s.view.floor

HasItems(v) == \E c \in Claims : v.items[c] # None

-----------------------------------------------------------------------------
(* `update`: prune, record, advertise, drop the feasibility distance,     *)
(* requests, show. Returns the node's new state, its messages per          *)
(* neighbor, and its reader's events.                                     *)

\* `prune` at the root interest: once no certifier's view has a floor, the
\* merged view is let go and rebuilt empty.
Prune(n, s, ov) ==
    LET c == Certified(s, TRUE) IN
    IF n # Origin /\ Selected(n, s, ov).on /\ c.ok /\ s.view.floor >= 0 /\ ~c.root
    THEN [s EXCEPT !.view = EmptyView]
    ELSE s

\* What `advertise` decides for one peer.
Plan(n, s, m, sel, live, av, ov) ==
    LET was == s.sent[m]
        reach0 == IF sel.on /\ Serves(n)
                  THEN Finite(sel.seq, sel.met, IF Hops THEN sel.hops \cup {n} ELSE {})
                  ELSE IF ~sel.on /\ Serves(n) /\ live /\ was.kind # "none" THEN HeldReach
                  ELSE NoReach
        \* A route starts again only once its last end was echoed.
        reach == IF was.kind # "none" \/ s.unacked[m] = 0 \/ Echo = "none" THEN reach0 ELSE NoReach
        meets == HasItems(av)
        send == IF reach.kind # "none" /\ meets THEN reach ELSE NoReach
        routeOut == IF send.kind # "none" /\ was # send THEN <<RouteMsg(send)>> ELSE <<>>
        batchGo == send.kind # "none"
                   \/ (reach.kind # "none" /\ was.kind # "none" /\ s.sentV[m].floor # Absent)
        prev == IF s.sentV[m].floor = Absent THEN EmptyView ELSE s.sentV[m]
        now == [floor |-> FloorOf(n, s, ov), items |-> av.items]
        batchOut == IF batchGo /\ now # prev THEN <<BatchMsg(now)>> ELSE <<>>
        endIt == send.kind = "none" /\ was.kind # "none"
    IN [sent |-> send,
        sentV |-> IF send.kind = "none" THEN NoView ELSE IF batchGo THEN now ELSE s.sentV[m],
        unacked |-> s.unacked[m] + (IF endIt /\ Echo # "none" THEN 1 ELSE 0),
        out |-> routeOut \o batchOut \o (IF endIt THEN <<RouteMsg(NoReach)>> ELSE <<>>)]

\* `record`: the feasibility distance for a route this node sends anywhere.
Record(n, s, sel, av, ups) ==
    LET sends == \E m \in Peers(n, ups) :
                    Serves(n) /\ (s.sent[m].kind # "none" \/ s.unacked[m] = 0) /\ HasItems(av)
    IN IF ~sel.on \/ ~sends THEN s
       ELSE IF ~s.fd.on THEN [s EXCEPT !.fd = [on |-> TRUE, seq |-> sel.seq, met |-> sel.met]]
       ELSE IF sel.seq > s.fd.seq \/ (sel.seq = s.fd.seq /\ sel.met < s.fd.met)
            THEN [s EXCEPT !.fd = [on |-> TRUE, seq |-> sel.seq, met |-> sel.met]]
       ELSE s

\* `requests`: start, resend, or clear the seqno request.
Requests(n, s, sel) ==
    IF n = Origin THEN [pending |-> NoPending, to |-> None, seq |-> 0]
    ELSE
    LET starved == ~sel.on /\ InfeasibleSet(s) # {}
        want == s.fd.seq + 1
        p0 == s.pending
        p1 == IF ~starved THEN p0
              ELSE IF ~p0.on THEN [on |-> TRUE, seq |-> want, from |-> {}, asked |-> None]
              ELSE IF p0.seq < want THEN [p0 EXCEPT !.seq = want, !.asked = None]
              ELSE p0
        answered == sel.on /\ sel.seq >= p1.seq
        hasTarget == sel.on \/ InfeasibleSet(s) # {}
        target == IF sel.on THEN sel.next ELSE Best(n, s, InfeasibleSet(s))
    IN IF ~p1.on THEN [pending |-> p1, to |-> None, seq |-> 0]
       ELSE IF answered \/ (p1.from = {} /\ ~starved) \/ ~hasTarget
            THEN [pending |-> NoPending, to |-> None, seq |-> 0]
       ELSE IF p1.asked # target
            THEN [pending |-> [p1 EXCEPT !.asked = target], to |-> target, seq |-> p1.seq]
       ELSE [pending |-> p1, to |-> None, seq |-> 0]

\* `shown_now`: how the reader sees each claim.
ShownNow(n, s, sel, live, ov) ==
    LET serving == Certified(s, FALSE)
        avail == sel.on /\ (n = Origin \/ ~serving.ok \/ serving.root)
        v == Adopted(n, s, ov)
    IN [c \in Claims |->
          IF v.items[c] = None THEN NotShown ELSE [item |-> v.items[c], avail |-> avail]]

\* `show`: the reader's events, as changes to `ended` and the counters.
\* A claim leaving while live and certified is an END, else it went offline.
Events(n, s, old, now, live, ov, eb) ==
    LET c0 == Certified(s, TRUE)
        uncertified == (c0.ok /\ ~c0.root) \/ (n # Origin /\ s.view.floor < 0)
        isEnd(c) == old[c].item # None /\ now[c].item = None /\ live /\ ~uncertified
        isStart(c) == old[c].item = None /\ now[c].item # None
        isUpdate(c) == old[c].item # None /\ now[c].item # None /\ old[c].item # now[c].item
    IN [ended |-> [c \in Claims |-> IF isEnd(c) THEN old[c].item ELSE eb[c]],
        revivals |-> Cardinality({c \in Claims : isStart(c) /\ eb[c] = now[c].item}),
        falseEnds |-> Cardinality({c \in Claims : isEnd(c) /\ ov.items[c] = old[c].item}),
        regressions |-> Cardinality({c \in Claims : isUpdate(c) /\ now[c].item < old[c].item})]

\* `update` on node n's state `s0`, with the origin's view `ov`, the open
\* sessions `ups`, and `eb`, the items n's reader saw END for. A prune forgets.
Update(n, s0, ov, ups, eb) ==
    LET s1 == Prune(n, s0, ov)
        eb1 == IF s1 # s0 THEN [c \in Claims |-> None] ELSE eb
        sel == Selected(n, s1, ov)
        s2 == Record(n, s1, sel, Adopted(n, s1, ov), ups)
        live == Live(n, s2)
        av == Adopted(n, s2, ov)
        plans == [m \in Peers(n, ups) |-> Plan(n, s2, m, sel, live, av, ov)]
        s3 == [s2 EXCEPT
                 !.sent = [m \in Nodes |-> IF m \in Peers(n, ups) THEN plans[m].sent ELSE s2.sent[m]],
                 !.sentV = [m \in Nodes |-> IF m \in Peers(n, ups) THEN plans[m].sentV ELSE s2.sentV[m]],
                 !.unacked = [m \in Nodes |-> IF m \in Peers(n, ups) THEN plans[m].unacked ELSE s2.unacked[m]]]
        \* Drop the feasibility distance once nothing routes, advertises, or awaits an echo.
        drained == n # Origin
                   /\ \A m \in Nodes : ~s3.rx[m].on /\ s3.sent[m].kind = "none" /\ s3.unacked[m] = 0
        s4 == IF drained THEN [s3 EXCEPT !.fd = NoFd] ELSE s3
        req == Requests(n, s4, sel)
        s5 == [s4 EXCEPT !.pending = req.pending]
        now == ShownNow(n, s5, sel, live, ov)
        ev == Events(n, s5, s0.shown, now, live, ov, eb1)
        \* A deferred echo stays undecided while this node holds its route.
        \* Once it selects another or has ended its own, the echo waits on
        \* every ROUTE_END this node has sent that awaits an echo, and on none
        \* it sends later. Echo what waits on nothing, ahead of what this
        \* update sends.
        unechoed == {m \in Nodes : s5.unacked[m] > 0}
        holding == ~sel.on /\ live
        owed == [m \in Nodes |->
                   IF s5.owed[m] = Undecided
                   THEN IF holding THEN Undecided ELSE [on |-> TRUE, wait |-> unechoed]
                   ELSE [s5.owed[m] EXCEPT !.wait = @ \cap unechoed]]
        ready(m) == owed[m].on /\ owed[m].wait = {}
        s6 == [s5 EXCEPT !.owed = [m \in Nodes |-> IF ready(m) THEN NotOwed ELSE owed[m]]]
        out == [m \in Nodes |->
                  (IF ready(m) THEN <<EchoMsg>> ELSE <<>>)
                  \o (IF m \in Peers(n, ups) THEN plans[m].out ELSE <<>>)
                  \o (IF m = req.to THEN <<RequestMsg(req.seq)>> ELSE <<>>)]
    IN [s |-> [s6 EXCEPT !.shown = now], out |-> out, ev |-> ev]

\* The origin ignores every ROUTE and batch it hears for its own identity, so
\* only ROUTE_REQUEST, ROUTE_END (which it echoes), and echoes are queued
\* toward it.
\* Dropping the rest changes no behavior and keeps the state space small.
Heeded(d, m) ==
    d[2] # Origin \/ m.type \in {"request", "echo"} \/ (m.type = "route" /\ m.reach.kind = "none")

\* Commit the updates `rs` (a function from nodes to `Update` results), appending
\* each node's messages to `q0`. Nodes in one step send on disjoint queues.
Commit(rs, q0) ==
    LET ns == DOMAIN rs
        Sum(f(_)) == LET F[S \in SUBSET ns] ==
                            IF S = {} THEN 0 ELSE LET x == CHOOSE x \in S : TRUE IN f(x) + F[S \ {x}]
                     IN F[ns]
    IN /\ st' = [n \in Nodes |-> IF n \in ns THEN rs[n].s ELSE st[n]]
       /\ q' = [d \in Dirs |-> IF d[1] \in ns THEN q0[d] \o SelectSeq(rs[d[1]].out[d[2]], LAMBDA m : Heeded(d, m))
                                 ELSE q0[d]]
       /\ ended' = [n \in Nodes |-> IF n \in ns THEN rs[n].ev.ended ELSE ended[n]]
       /\ revivals' = revivals + Sum(LAMBDA n : rs[n].ev.revivals)
       /\ falseEnds' = falseEnds + Sum(LAMBDA n : rs[n].ev.falseEnds)
       /\ regressions' = regressions + Sum(LAMBDA n : rs[n].ev.regressions)

-----------------------------------------------------------------------------
(* Initial state: every session open (`Sim::join`), the origin declared     *)
(* with no claims (`originate`), and nothing sent.                         *)

InitNode == [
    rx |-> [m \in Nodes |-> NoEntry],
    fd |-> NoFd,
    heard |-> [m \in Nodes |-> NoHeard],
    view |-> NoView,
    sent |-> [m \in Nodes |-> NoReach],
    sentV |-> [m \in Nodes |-> NoView],
    unacked |-> [m \in Nodes |-> 0],
    owed |-> [m \in Nodes |-> NotOwed],
    pending |-> NoPending,
    shown |-> [c \in Claims |-> NotShown]
]

Init ==
    /\ up = Links
    /\ q = [d \in Dirs |-> <<>>]
    /\ rev = 0
    /\ own = NoItems
    /\ seqno = 0
    /\ hist = <<NoItems>>
    /\ st = [n \in Nodes |-> InitNode]
    /\ ended = [n \in Nodes |-> [c \in Claims |-> None]]
    /\ revivals = 0
    /\ falseEnds = 0
    /\ regressions = 0
    /\ flaps = 0
    /\ collects = 0

-----------------------------------------------------------------------------
(* The origin changes a claim (`announce` / `unannounce` -> `change`).      *)

ChangeClaim(c, item) ==
    LET own1 == [own EXCEPT ![c] = item]
        ov1 == [rev |-> rev + 1, items |-> own1, seq |-> seqno]
    IN /\ rev' = rev + 1
       /\ own' = own1
       /\ hist' = Append(hist, own1)
       /\ Commit([n \in {Origin} |-> Update(Origin, st[Origin], ov1, up, ended[Origin])], q)
       /\ UNCHANGED <<up, seqno, flaps, collects>>

\* A new claim, or new attributes for one, under a fresh revision.
Announce(c) == rev < MaxRev /\ ChangeClaim(c, rev + 1)
Unannounce(c) == rev < MaxRev /\ own[c] # None /\ ChangeClaim(c, None)

-----------------------------------------------------------------------------
(* A message arrives (`Sim::deliver`).                                      *)

\* `View::restrict` at the root: a view with no floor certifies no claim.
Certify(v) == IF v.floor >= 0 THEN v ELSE [floor |-> v.floor, items |-> NoItems]

\* `route`: ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD, or ROUTE_END. After an
\* end, the next batch starts from an empty view, and the last view is kept
\* for what it certified.
OnRoute(n, s, from, reach) ==
    LET h == s.heard[from]
        rx1 == CASE reach.kind = "fin" ->
                      [s.rx EXCEPT ![from] = [on |-> TRUE, seq |-> reach.seq,
                                              met |-> reach.met, held |-> FALSE,
                                              hops |-> reach.hops, mine |-> n \in reach.hops]]
                 [] reach.kind = "held" ->
                      IF s.rx[from].on THEN [s.rx EXCEPT ![from].held = TRUE] ELSE s.rx
                 [] OTHER -> [s.rx EXCEPT ![from] = NoEntry]
        kept == Certify(IF h.kept.floor # Absent THEN h.kept ELSE h.raw)
        heard1 == IF reach.kind = "none" /\ h.on
                  THEN [s.heard EXCEPT ![from] =
                          [on |-> TRUE, raw |-> EmptyView, kept |-> kept, ended |-> TRUE]]
                  ELSE s.heard
    IN [s EXCEPT !.rx = rx1, !.heard = heard1]

\* `View::merge` at the root: take a view certified at a newer floor whole.
Merge(mine, theirs) == IF theirs.floor >= 0 /\ theirs.floor > mine.floor THEN theirs ELSE mine

\* `batch`: the batch carries the sender's whole presentation, which is what
\* applying its changes to the last view gives on a FIFO stream. It replaces
\* the neighbor's view, which merges into this node's.
OnBatch(s, from, v) ==
    [s EXCEPT !.heard[from] = [on |-> TRUE, raw |-> v, kept |-> NoView, ended |-> FALSE],
              !.view = Merge(IF s.view.floor = Absent THEN EmptyView ELSE s.view, v)]

\* `request` at a relay that has not already advertised that seqno.
OnRequest(s, from, seq) ==
    LET p == IF s.pending.on THEN s.pending
             ELSE [on |-> TRUE, seq |-> seq, from |-> {}, asked |-> None]
        p1 == IF seq > p.seq THEN [p EXCEPT !.seq = seq, !.asked = None] ELSE p
    IN [s EXCEPT !.pending = [p1 EXCEPT !.from = @ \cup {from}]]

Max(a, b) == IF a > b THEN a ELSE b

\* The next message of a kind on a queue: its head, or with `EchoLane` the
\* first echo or the first message that is not one.
IsEcho(msg) == msg.type = "echo"
Front(sq, echo) ==
    IF EchoLane
    THEN IF \E i \in DOMAIN sq : IsEcho(sq[i]) = echo
         THEN CHOOSE i \in DOMAIN sq : IsEcho(sq[i]) = echo /\ \A j \in 1..(i - 1) : IsEcho(sq[j]) # echo
         ELSE 0
    ELSE IF sq # <<>> /\ IsEcho(sq[1]) = echo THEN 1 ELSE 0
Without(sq, i) == SubSeq(sq, 1, i - 1) \o SubSeq(sq, i + 1, Len(sq))

Deliver(d) ==
    LET m == d[1]
        n == d[2]
        at == Front(q[d], FALSE)
        msg == q[d][at]
        isEnd == msg.type = "route" /\ msg.reach.kind = "none"
        sel == Selected(n, st[n], OV)
        \* A ROUTE_END from the neighbor this node routed through is echoed
        \* by `Update`, once nothing this node ended awaits an echo. Any
        \* other is echoed at once, ahead of what the update sends.
        defer == isEnd /\ Transitive /\ Serves(n) /\ Through(n, st[n], m, sel)
        q0 == [q EXCEPT ![d] = Without(@, at),
                        ![<<n, m>>] = IF isEnd /\ ~defer /\ Echo # "none" THEN Append(@, EchoMsg) ELSE @]
        \* Only the origin advances its seqno to answer a request.
        ov1 == IF n = Origin /\ msg.type = "request"
               THEN [OV EXCEPT !.seq = Max(seqno, msg.seq)] ELSE OV
        \* Already advertised to the requester: `request` returns before `update`.
        skip == n # Origin /\ msg.type = "request" /\ sel.on /\ sel.seq >= msg.seq
        \* The origin's own identity merges nothing it hears.
        s1 == IF n = Origin THEN st[n]
              ELSE CASE msg.type = "route" ->
                          [OnRoute(n, st[n], m, msg.reach) EXCEPT !.owed[m] = IF defer THEN Undecided ELSE @]
                     [] msg.type = "batch" -> OnBatch(st[n], m, msg.view)
                     [] OTHER -> OnRequest(st[n], m, msg.seq)
    IN /\ at > 0
       /\ seqno' = ov1.seq
       /\ IF skip
          THEN /\ q' = q0
               /\ UNCHANGED <<st, ended, revivals, falseEnds, regressions>>
          ELSE Commit([x \in {n} |-> Update(n, s1, ov1, up, ended[n])], q0)
       /\ UNCHANGED <<up, rev, own, hist, flaps, collects>>

\* `echo`: an echoed ROUTE_END comes back, behind the echoer's earlier routes.
DeliverEcho(d) ==
    LET m == d[1]
        n == d[2]
        at == Front(q[d], TRUE)
        q0 == [q EXCEPT ![d] = Without(@, at)]
    IN /\ at > 0
       /\ IF st[n].unacked[m] = 0
          THEN /\ q' = q0
               /\ UNCHANGED <<st, ended, revivals, falseEnds, regressions>>
          ELSE Commit([x \in {n} |->
                  Update(n, [st[n] EXCEPT !.unacked[m] = @ - 1], OV, up, ended[n])], q0)
       /\ UNCHANGED <<up, rev, own, seqno, hist, flaps, collects>>

-----------------------------------------------------------------------------
(* Sessions open and close (`Sim::connect`, `Sim::disconnect`).            *)

\* `peer_interest`: a new stream from `y` gets a complete snapshot.
Opened(s, y) == [s EXCEPT !.sentV[y] = NoView]

\* `disconnect`: everything learned on the session goes, and it counts as the
\* echo of every ROUTE_END sent on it.
Closed(s, y) ==
    [s EXCEPT !.rx[y] = NoEntry,
              !.heard[y] = NoHeard,
              !.unacked[y] = 0,
              !.owed[y] = NotOwed,
              !.sent[y] = NoReach,
              !.sentV[y] = NoView,
              !.pending.from = @ \ {y}]

Connect(l) ==
    LET up1 == up \cup {l}
    IN /\ l \notin up
       /\ flaps < MaxFlaps
       /\ up' = up1
       /\ flaps' = flaps + 1
       /\ Commit([x \in l |->
               LET y == CHOOSE y \in l : y # x
               IN Update(x, Opened(st[x], y), OV, up1, ended[x])], q)
       /\ UNCHANGED <<rev, own, seqno, hist, collects>>

Disconnect(l) ==
    LET up1 == up \ {l}
        q0 == [d \in Dirs |-> IF {d[1], d[2]} = l THEN <<>> ELSE q[d]]
    IN /\ l \in up
       /\ flaps < MaxFlaps
       /\ up' = up1
       /\ flaps' = flaps + 1
       /\ Commit([x \in l |->
               LET y == CHOOSE y \in l : y # x
               IN Update(x, Closed(st[x], y), OV, up1, ended[x])], q0)
       /\ UNCHANGED <<rev, own, seqno, hist, collects>>

\* `collect`, allowed once the node holds no route (`Sim::holds`). It
\* forgets the views, so the reader's earlier ENDs no longer count.
Collect(n) ==
    /\ collects < MaxCollects
    /\ \A m \in Nodes : ~st[n].rx[m].on
    /\ collects' = collects + 1
    /\ Commit([x \in {n} |->
            Update(n, [st[n] EXCEPT !.view = NoView,
                                    !.heard = [m \in Nodes |-> NoHeard],
                                    !.pending = NoPending],
                   OV, up, [c \in Claims |-> None])], q)
    /\ UNCHANGED <<up, rev, own, seqno, hist, flaps>>

Quiet == \A d \in Dirs : q[d] = <<>>

\* Nothing is in flight and no more churn is allowed: the run is over.
Done ==
    /\ Quiet
    /\ rev = MaxRev \/ flaps = MaxFlaps
    /\ UNCHANGED vars

Next ==
    \/ \E c \in Claims : Announce(c) \/ Unannounce(c)
    \/ \E d \in Dirs : Deliver(d) \/ DeliverEcho(d)
    \/ \E l \in Links : Connect(l) \/ Disconnect(l)
    \/ \E n \in Relays : Collect(n)
    \/ Done

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules.                                                                  *)

\* Every view a relay keeps is the origin's claims at that view's revision.
Truthful ==
    \A n \in Relays :
        /\ st[n].view.floor >= 0 => st[n].view.items = hist[st[n].view.floor + 1]
        /\ \A m \in Nodes :
              LET v == HeardView(st[n].heard[m])
              IN st[n].heard[m].on /\ v.floor >= 0 => v.items = hist[v.floor + 1]

\* At the root interest every claim is certified, so `loose` is empty.
AllCertified ==
    /\ \A n \in Relays :
          /\ st[n].view.floor < 0 => ~HasItems(st[n].view)
          /\ \A m \in Nodes : LET v == HeardView(st[n].heard[m]) IN v.floor < 0 => ~HasItems(v)
    /\ \A d \in Dirs : \A i \in DOMAIN q[d] :
          q[d][i].type = "batch" => (q[d][i].view.floor >= 0 \/ ~HasItems(q[d][i].view))

\* A reader never sees a broadcast start again after it saw it end.
NoRevival == revivals = 0

\* A reader never sees a broadcast end while the origin still has it.
NoFalseEnd == falseEnds = 0

\* A reader never sees an older version replace a newer one.
NoRegression == regressions = 0

NextHop(n) == IF n = None THEN None ELSE Selected(n, st[n], OV).next
RECURSIVE Hop(_, _)
Hop(n, k) == IF k = 0 THEN n ELSE Hop(NextHop(n), k - 1)

\* Following selected next hops from any relay never cycles.
LoopFree == \A n \in Relays : Hop(n, Cardinality(Nodes)) \notin Relays

\* The costliest loop-free path from the origin to n through nodes that
\* forward: no route n selects can be longer without having looped.
RECURSIVE Paths(_, _)
Paths(p, k) ==
    IF k = 0 THEN {p}
    ELSE {p} \cup UNION {Paths(Append(p, x), k - 1) :
                          x \in {y \in Nodes : {p[Len(p)], y} \in Links
                                               /\ \A i \in DOMAIN p : p[i] # y
                                               /\ (Len(p) = 1 \/ Serves(p[Len(p)]))}}
RECURSIVE PathCost(_)
PathCost(p) == IF Len(p) < 2 THEN 0 ELSE Cost[{p[1], p[2]}] + PathCost(Tail(p))
LongestTable == [n \in Nodes |->
    LET ps == {p \in Paths(<<Origin>>, Cardinality(Nodes) - 1) : p[Len(p)] = n}
    IN IF ps = {} THEN 0 ELSE CHOOSE c \in {PathCost(p) : p \in ps} : \A p \in ps : PathCost(p) <= c]
Longest(n) == LongestTable[n]

\* No relay selects a route longer than any loop-free path: a longer one is a
\* stale route counting up as it bounces between relays (Babel's feasibility
\* condition exists to rule this out).
NoCountToInfinity == \A n \in Relays : Selected(n, st[n], OV).met <= Longest(n)

\* Explore one step past NoCountToInfinity and no further, so a count to
\* infinity is reported rather than explored forever: what m advertises is a
\* metric m selected.
Bounded == \A n, m \in Nodes : st[n].rx[m].met <= Longest(m)

\* Nodes the origin reaches over open sessions, through nodes that forward.
RECURSIVE Grow(_, _)
Grow(R, k) == IF k = 0 THEN R
              ELSE Grow(R \cup {n \in Nodes : \E m \in R : Serves(m) /\ {m, n} \in up}, k - 1)
Reached == Grow({Origin}, Cardinality(Nodes))

\* Once nothing is in flight, each reached relay shows exactly the origin's
\* claims, available, and routes only while it has any; each cut-off relay
\* shows nothing and keeps no route or feasibility distance; and no seqno
\* request is pending.
Settled ==
    /\ \A n \in Relays \cap Reached :
          /\ \A c \in Claims :
                st[n].shown[c] = IF own[c] = None THEN NotShown
                                 ELSE [item |-> own[c], avail |-> TRUE]
          /\ Selected(n, st[n], OV).on = HasItems([items |-> own])
    /\ \A n \in Relays \ Reached :
          /\ \A c \in Claims : st[n].shown[c] = NotShown
          /\ \A m \in Nodes : ~st[n].rx[m].on
          /\ ~st[n].fd.on
    /\ \A n \in Nodes : ~st[n].pending.on

QuietIsSettled == Quiet => Settled

\* Once nothing is in flight, no ROUTE_END awaits an echo and no echo is
\* owed: echoes that wait on each other never wait forever.
QuietIsEchoed ==
    Quiet => \A n, m \in Nodes : st[n].unacked[m] = 0 /\ ~st[n].owed[m].on

=============================================================================
