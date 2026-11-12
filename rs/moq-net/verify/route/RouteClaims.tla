---------------------------- MODULE RouteClaims ----------------------------
(***************************************************************************)
(* What RouteHandoff.tla's relays show their readers, with no paths: on    *)
(* every step, any relay's routes may come and go as they like. A relay    *)
(* hears a route per neighbour and key, believed or not, and holds a route *)
(* of its own per key, live, held, or none. The claims a node sends may    *)
(* reach any relay, in any order, any number of times, or never.          *)
(*                                                                         *)
(* RouteHandoff refines this model (its `RefinesClaims`), so the claim     *)
(* rules here hold there too. Apalache checks that `IndInv` holds at       *)
(* `Init` and that every step keeps it, so they hold for runs of any       *)
(* length: revisions, sessions, and route changes are unbounded.           *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Apalache

CONSTANTS
    \* @type: Set(Int);
    Nodes,     \* relays and the publisher
    \* @type: Int;
    Origin,    \* the publisher
    \* @type: Set(Str);
    Claims,    \* claim paths
    \* @type: Set(Str);
    Narrow,    \* the claims inside prefix "a"
    \* @type: Int;
    MaxFlight  \* `IndInit` only: the claims messages in flight

\* Apalache's names for the records below.
\* @typeAlias: floor = { rev: Int, items: Str -> Int };
\* @typeAlias: view = { floor: Int, items: Str -> Int };
\* @typeAlias: msg = { k: Str, rev: Int, items: Str -> Int };
\* @typeAlias: shown = { item: Int, avail: Bool };
\* Every relay's state is one function keyed by relay first, since Apalache
\* cannot pick a function of functions.
\* @typeAlias: heard = <<Int, Int, Str>> -> Str;
\* @typeAlias: offered = <<Int, Int, Str>> -> $floor;
\* @typeAlias: route = <<Int, Str>> -> Str;
\* @typeAlias: views = <<Int, Str>> -> $view;
RouteClaims_aliases == TRUE

Relays == Nodes \ {Origin}

\* A claim's item, the revision that last set it, or None when absent.
None == -1
\* A view's floor: a revision, None for no floor yet, or Absent for no view.
Absent == -2

Keys == {"root", "a"}
\* @type: (Str, Str) => Bool;
Covers(k, c) == k = "root" \/ c \in Narrow
\* @type: Str => Str;
KeyOf(c) == IF c \in Narrow THEN "a" ELSE "root"

NoItems == [c \in Claims |-> None]
\* @type: (Str -> Int, Str) => Str -> Int;
Restrict(items, k) == [c \in Claims |-> IF Covers(k, c) THEN items[c] ELSE None]
NoFloor == [rev |-> None, items |-> NoItems]
EmptyView == [floor |-> None, items |-> NoItems]
NoView == [floor |-> Absent, items |-> NoItems]
NotShown == [item |-> None, avail |-> FALSE]

\* What a relay heard from a neighbour for a key: nothing, a route it
\* believes, or one it does not (held, dead, or through itself).
Hearings == {"off", "live", "doubt"}
\* The route a relay uses for a key.
Routings == {"none", "held", "live"}

\* @type: Set(<<Int, Int, Str>>);
HeardIdx == Relays \X Nodes \X Keys
\* @type: Set(<<Int, Str>>);
KeyIdx == Relays \X Keys
\* @type: Set(<<Int, Str>>);
ClaimIdx == Relays \X Claims

VARIABLES
    \* @type: Int;
    rev,         \* the publisher's claim revision
    \* @type: Str -> Int;
    own,         \* the publisher's claims
    \* @type: Int -> (Str -> Int);
    hist,        \* hist[r]: the publisher's claims at revision r, for each r the state names
    \* @type: $heard;
    heard,       \* heard[<<n, m, k>>]: the route n heard from m for key k
    \* @type: $offered;
    offered,     \* offered[<<n, m, k>>]: the floor that route last gave n
    \* @type: $route;
    route,       \* route[<<n, k>>]: the route n uses for key k
    \* @type: $views;
    view,        \* view[<<n, k>>]: the newest floor a route covering k gave n
    \* @type: <<Int, Str>> -> $shown;
    shown,       \* shown[<<n, c>>]: what n's reader sees of claim c
    \* @type: <<Int, Str>> -> Int;
    ended,       \* ended[<<n, c>>]: the item n's reader last saw END for, until n forgets c
    \* @type: Set($msg);
    flight,      \* the claims messages in flight
    \* @type: Int;
    revivals,    \* reader STARTs of an item the reader saw END for
    \* @type: Int;
    falseEnds,   \* reader ENDs while the publisher still has that item
    \* @type: Int;
    regressions  \* reader UPDATEs to an older item

vars == <<rev, own, hist, heard, offered, route, view, shown, ended, flight,
          revivals, falseEnds, regressions>>

-----------------------------------------------------------------------------
(* What a relay shows, as RouteHandoff's `Know`, `ShownNow`, and `Events`.  *)

\* @type: ($route, Int, Str) => Bool;
Presence(rt, n, k) == rt[<<n, k>>] # "none" \/ (k = "a" /\ rt[<<n, "root">>] # "none")

\* Relay n's view for key k: the newest floor a believed route covering k
\* gave, kept while n holds a route covering k.
\* @type: ($heard, $offered, $route, $views, Int, Str) => $view;
NewView(hd, of, rt, vw, n, k) ==
    IF ~Presence(rt, n, k) THEN NoView
    ELSE LET \* @type: ($view, <<Int, Int, Str>>) => $view;
             Newer(v, x) ==
                IF hd[x] = "live" /\ of[x].rev > v.floor
                THEN [floor |-> of[x].rev, items |-> Restrict(of[x].items, k)]
                ELSE v
         IN ApaFoldSet(Newer, IF vw[<<n, k>>].floor = Absent THEN EmptyView ELSE vw[<<n, k>>],
                       {n} \X Nodes \X {k, "root"})

\* What relay n's reader sees of claim c.
\* @type: ($route, $views, Int, Str) => $shown;
ShownNow(rt, vw, n, c) ==
    LET v == vw[<<n, KeyOf(c)>>] IN
    IF v.floor = Absent \/ v.items[c] = None THEN NotShown
    ELSE [item |-> v.items[c],
          avail |-> rt[<<n, "root">>] = "live" \/ (c \in Narrow /\ rt[<<n, "a">>] = "live")]

\* What node n sends: its view per key, or the publisher's claims.
\* @type: (Int, Int, Str -> Int, $views) => Set($msg);
Sends(n, rv, ow, vw) ==
    IF n = Origin THEN {[k |-> k, rev |-> rv, items |-> Restrict(ow, k)] : k \in Keys}
    ELSE {[k |-> k, rev |-> IF vw[<<n, k>>].floor = Absent THEN None ELSE vw[<<n, k>>].floor,
           items |-> vw[<<n, k>>].items] : k \in Keys}

\* The revisions the state names: the publisher's, and every floor's.
\* @type: (Int, $offered, $views, Set($msg)) => Set(Int);
Refs(rv, of, vw, fl) ==
    {r \in {rv}
           \cup {vw[x].floor : x \in KeyIdx}
           \cup {of[x].rev : x \in HeardIdx}
           \cup {y.rev : y \in fl} : r >= 0}

-----------------------------------------------------------------------------
(* The publisher's claims and every relay's routes start empty; the        *)
(* publisher's first claims may be in flight.                              *)

Init ==
    /\ rev = 0
    /\ own = NoItems
    /\ hist = [r \in {0} |-> NoItems]
    /\ heard = [x \in HeardIdx |-> "off"]
    /\ offered = [x \in HeardIdx |-> NoFloor]
    /\ route = [x \in KeyIdx |-> "none"]
    /\ view = [x \in KeyIdx |-> NoView]
    /\ shown = [x \in ClaimIdx |-> NotShown]
    /\ ended = [x \in ClaimIdx |-> None]
    /\ flight \in SUBSET Sends(Origin, 0, NoItems, view)
    /\ revivals = 0
    /\ falseEnds = 0
    /\ regressions = 0

\* The publisher keeps its claims or changes one.
Publish ==
    \/ rev' = rev /\ own' = own
    \/ \E c \in Claims, item \in {rev + 1, None} :
          /\ item = None => own[c] # None
          /\ rev' = rev + 1
          /\ own' = [own EXCEPT ![c] = item]

\* One step: the publisher may change a claim, and every relay's routes may
\* change. A route that stays on may take any claims in flight to it under
\* its key; one that turns on or off has no floor. Every relay then updates
\* its views and shows its reader the difference, and every node may send
\* its views, while any message in flight may be lost.
Next ==
    /\ Publish
    /\ heard' \in [HeardIdx -> Hearings]
    /\ route' \in [KeyIdx -> Routings]
    /\ LET arrived == {[rev |-> y.rev, items |-> y.items] : y \in flight}
           pool == {NoFloor} \cup arrived \cup {offered[x] : x \in HeardIdx}
       IN /\ offered' \in [HeardIdx -> pool]
          /\ \A x \in HeardIdx :
                IF heard[x] = "off" \/ heard'[x] = "off"
                THEN offered'[x] = NoFloor
                ELSE \/ offered'[x] = offered[x]
                     \/ \E y \in flight :
                           y.k = x[3] /\ offered'[x] = [rev |-> y.rev, items |-> y.items]
    /\ view' = [x \in KeyIdx |-> NewView(heard', offered', route', view, x[1], x[2])]
    /\ shown' = [x \in ClaimIdx |-> ShownNow(route', view', x[1], x[2])]
    /\ LET \* @type: (<<Int, Str>>) => Bool;
           present(x) == Presence(route', x[1], KeyOf(x[2]))
           \* @type: (<<Int, Str>>) => Bool;
           isEnd(x) == shown[x].item # None /\ shown'[x].item = None /\ present(x)
           \* @type: (<<Int, Str>>) => Bool;
           isStart(x) == shown[x].item = None /\ shown'[x].item # None
           \* @type: (<<Int, Str>>) => Bool;
           isUpdate(x) == shown[x].item # None /\ shown'[x].item # None /\ shown[x].item # shown'[x].item
       IN /\ ended' = [x \in ClaimIdx |->
                        IF isEnd(x) THEN shown[x].item
                        ELSE IF present(x) THEN ended[x] ELSE None]
          /\ revivals' = revivals + Cardinality({x \in ClaimIdx : isStart(x) /\ ended[x] = shown'[x].item})
          /\ falseEnds' = falseEnds + Cardinality({x \in ClaimIdx : isEnd(x) /\ own'[x[2]] = shown[x].item})
          /\ regressions' = regressions + Cardinality({x \in ClaimIdx : isUpdate(x) /\ shown'[x].item < shown[x].item})
    /\ flight' \in SUBSET (flight \cup UNION {Sends(n, rev', own', view') : n \in Nodes})
    /\ hist' = [r \in Refs(rev', offered', view', flight') |-> IF r = rev' THEN own' ELSE hist[r]]

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Rules, as RouteHandoff states them.                                     *)

\* Every floor a relay keeps or was offered is the publisher's claims at that
\* revision, within its key.
Truthful ==
    /\ \A x \in KeyIdx : view[x].floor >= 0 =>
          view[x].items = Restrict(hist[view[x].floor], x[2])
    /\ \A x \in HeardIdx : heard[x] # "off" /\ offered[x].rev >= 0 =>
          offered[x].items = Restrict(hist[offered[x].rev], x[3])

\* A reader never sees a broadcast start again after it saw it end.
NoRevival == revivals = 0

\* A reader never sees a broadcast end while the publisher still has it.
NoFalseEnd == falseEnds = 0

\* A reader never sees an older version replace a newer one.
NoRegression == regressions = 0

-----------------------------------------------------------------------------
(* The inductive invariant. A floor only grows while a route covers it,    *)
(* each floor is the publisher's claims at its revision, and an item is    *)
(* the revision that set it, so a newer floor never names an older item.   *)

\* A floor: None with no items, or a revision the publisher has passed with
\* its claims then, within key k.
\* @type: (Str, Int, Str -> Int) => Bool;
Sound(k, r, items) ==
    \/ r = None /\ items = NoItems
    \/ /\ 0 <= r /\ r <= rev
       /\ r \in DOMAIN hist
       /\ items = Restrict(hist[r], k)

IndInv ==
    /\ Truthful
    /\ NoRevival
    /\ NoFalseEnd
    /\ NoRegression
    \* The history: an item is the revision that set it, and it stays set
    \* until a later revision changes it.
    /\ rev >= 0
    /\ DOMAIN hist = Refs(rev, offered, view, flight)
    /\ hist[rev] = own
    /\ \A r \in DOMAIN hist, c \in Claims :
          hist[r][c] = None \/ (1 <= hist[r][c] /\ hist[r][c] <= r)
    /\ \A r1, r2 \in DOMAIN hist, c \in Claims :
          r1 <= r2 /\ hist[r2][c] # None /\ hist[r2][c] <= r1 => hist[r1][c] = hist[r2][c]
    \* Every floor is sound, and a route that is off gave none.
    /\ \A y \in flight : y.k \in Keys /\ Sound(y.k, y.rev, y.items)
    /\ \A x \in HeardIdx :
          /\ Sound(x[3], offered[x].rev, offered[x].items)
          /\ heard[x] = "off" => offered[x] = NoFloor
    \* A relay keeps a view exactly while it holds a route covering it.
    /\ \A x \in KeyIdx :
          IF Presence(route, x[1], x[2]) THEN Sound(x[2], view[x].floor, view[x].items)
          ELSE view[x] = NoView
    /\ \A x \in ClaimIdx : shown[x] = ShownNow(route, view, x[1], x[2])
    \* What the reader saw end is older than the view, which no longer has it.
    /\ \A x \in ClaimIdx :
          ended[x] # None =>
              LET v == view[<<x[1], KeyOf(x[2])>>] IN
              /\ Presence(route, x[1], KeyOf(x[2]))
              /\ 1 <= ended[x] /\ ended[x] <= v.floor
              /\ v.items[x[2]] # ended[x]

\* Any state of the right shape where `IndInv` holds, with up to `MaxFlight`
\* claims messages in flight.
IndInit ==
    /\ rev \in Int
    /\ own \in [Claims -> Int]
    /\ heard \in [HeardIdx -> Hearings]
    /\ route \in [KeyIdx -> Routings]
    /\ \E r \in [HeardIdx -> Int], i \in [HeardIdx \X Claims -> Int] :
          offered = [x \in HeardIdx |-> [rev |-> r[x], items |-> [c \in Claims |-> i[<<x, c>>]]]]
    /\ \E f \in [KeyIdx -> Int], i \in [KeyIdx \X Claims -> Int] :
          view = [x \in KeyIdx |-> [floor |-> f[x], items |-> [c \in Claims |-> i[<<x, c>>]]]]
    /\ \E i \in [ClaimIdx -> Int], a \in [ClaimIdx -> BOOLEAN] :
          shown = [x \in ClaimIdx |-> [item |-> i[x], avail |-> a[x]]]
    /\ ended \in [ClaimIdx -> Int]
    /\ flight = Gen(MaxFlight)
    /\ \A y \in flight : DOMAIN y.items = Claims
    /\ \E i \in [Refs(rev, offered, view, flight) \X Claims -> Int] :
          hist = [r \in Refs(rev, offered, view, flight) |-> [c \in Claims |-> i[<<r, c>>]]]
    /\ revivals \in Int
    /\ falseEnds \in Int
    /\ regressions \in Int
    /\ IndInv

=============================================================================
