# The route rule's convergence proof

The multicast route rule (`rs/moq-net/src/route/tests/multi.rs` with the epic's
`floor`, `epoch`, `batch`, `drain`, and `claimed`) stated as an increasing path
algebra in Agda, with Daggitt and Griffin's Theorem 5 applied to it
([arXiv 2106.01184](https://arxiv.org/abs/2106.01184), library
[MatthewDaggitt/agda-routing](https://github.com/MatthewDaggitt/agda-routing)):
on every network, from every state, under every schedule of delayed,
reordered, lost, and repeated messages, the asynchronous iteration reaches the
network's unique fixed point once the network holds still long enough.

`just test route-proof` type-checks it with its own Agda pin
(`nix/route-proof.nix`: Agda 2.6.4.3, standard-library 2.0, agda-routing at a
fixed commit), so neither the dev shell nor `nix flake check` fetches it. Nightly
runs it. Every module is `--safe` and nothing is postulated. The check takes
about a minute and a half; building the library the first time adds about 40 s.

| Module | What it holds |
|---|---|
| `Order.agda` | Decidable total orders and the combinators the rank is built from. |
| `Increasing.agda` | The algebra both levels share, and the theorem it buys (`convergent`, `converges-n²`). |
| `Route.agda` | Routes between forwarding nodes: every filter and the link a node holds. |
| `Claim.agda` | Claims across trust boundaries: the same algebra at the grain of the nodes that hand a claim on. |
| `Sim.agda` | `Route` with nodes named as the Rust simulation names them. |
| `Vectors.agda` | Results the Rust prototype computed, each a `refl` (generated). |
| `Accelerate.agda` | The floor and the named drains leave the rule's fixed point unchanged. |

## The algebra

A route is invalid or carries whether it is draining, a penalty (127 minus the
lowest priority on its path), a cost, whether a marked link is on its path, its
path, and whatever else a level carries (`extra`). Routes rank by draining,
then penalty, then cost, then unmarked before marked, then the shorter path,
then a fixed order on paths (the neighbour first), then `extra`. A node keeps the
lower of two routes. Crossing a link refuses a route whose path does not start
at the link's far end or already names its near end, and whatever the link's
filter refuses; otherwise it can only make the route draining, raise its
penalty, add to its cost, mark it, and prepend the link. That is an increasing
path algebra, which is all Theorem 5 asks.

For routes, the link from i to its neighbour j carries: whether it is up, i's
port cost and penalty, whether j sent GOAWAY, the nodes i knows draining, i's
session floors, whether i or j marked the link, whether j announces a claim,
and i's session number. It refuses a route when the link is down, when the
route is j's route to itself and j announces no claim, when the route is
marked and j marked the link, when the path would grow past 32 hops, or when
the path names a session i's floors hold closed. A route that names a node
known draining ranks as draining. Nodes are incarnations: a restart is a new
node.

## Coverage

**Mechanized** (Agda, `--safe`):

- The generic algebra is an increasing path algebra, so it converges, and the
  synchronous iteration within n² rounds (`Increasing.convergent`,
  `converges-n²`). This covers: selection is one total order with a fixed
  tie-break; crossing a session never makes a route preferred; a path naming
  its receiver is invalid and a path grows by exactly the hop it crossed; each
  filter maps a route to invalid; the named drain keeps one order.
- The route instance with every filter: claimed, the mark's export filter, the
  hop limit, the floor with epochs, and the named drain (`Route`).
- The claim instance: any chain of steps that only add, and refuse a claim
  naming the node that takes it, converges too (`Claim`). Echoes never beat
  the claims they echo because every step is increasing.
- The floor and the named drains only make the rule arrive sooner: the fixed
  point of the rule without them is a fixed point with them, given that no floor
  covers a live session (`FloorsClosed`) and that a node is named draining only
  once it sent GOAWAY on every live link (`DrainNamed`)
  (`Accelerate.accelerated-fixed`).

**Bound to the prototype by tests** (`rs/moq-net/src/route/tests/algebra.rs`):
the router's own functions (`Offer::own`, `Offer::onward`, `Here::arrive`,
`order`) meet the axioms on random links and routes, and 322 of their results
(200 single steps and 122 choices between two routes, half of them ties) are
written into `Vectors.agda`, where Agda must compute the same results by `refl`.
A Rust test fails when the file is stale (`ROUTE_VECTORS=write` regenerates
it), and a changed Agda definition fails the nightly check. Mutating any of the
floor, epoch, drain set, mark filter, hop limit, claimed, penalty, cost, drains,
tie-break, tag, or sessions is caught by a vector. This is a sample: no test
shows the Rust functions equal the Agda definitions on every input.

**Argued on paper** (below): the fixed point is unique with and without the
accelerators; the two hypotheses hold in the prototype; advertising once per
burst changes no fixed point; what a node advertises is a function of the
latest it heard; the dead-hop and ended withdrawals remove only what the floor
does; `claimed` removes only routes no request names; claims settle once routes
do; the tree is loop-free at the fixed point; and the rule as it runs, with the
next hop in use, still settles.

**Only tested**: the rule as it runs settles, on the rank and mark the fixed
order gives (`sticky.rs`); the floor refuses every route the dead-hop
withdrawal ends (`algebra.rs`, `the_floor_refuses_every_dead_hop`); a grant
never improves a claim in the real router (`a_grant_never_improves_a_claim`).

## What this does not give

- **The rule as it runs.** The theorem is for ties broken by one fixed order.
  The prototype puts the next hop in use ahead of the shorter path and the
  neighbour, so which of two equal routes a node settles on depends on which
  came first, and the stable state is not unique. The argument that it still
  settles is on paper and tested, not mechanized (below).
- **Loop freedom at every instant.** Only the fixed point is loop-free.
  [Requests](https://github.com/moq-dev/moq/pull/8881) name their origin node
  and fail closed instead.
- **A cost that depends on who subscribed.** Warmth is not in the algebra;
  [warmth](/quest/m2/multicast/warmth.md) argues it separately.
- **Liveness under churn that never stops.** Convergence needs each epoch to
  last long enough.
- **The chain as Rust records it.** The claim instance's path names every node
  that took the claim. Rust's chain names only the receivers across scoped
  grants, not the relay that exported the claim or the one that announced it,
  so a node refuses fewer echoes than the algebra does. The router should
  record the announcing relay first, then each grant's exporter and adopter,
  and refuse a claim that names it in the role it would take.
- **The neighbour key.** The proof orders equal routes by the neighbour's relay
  and then its start. The prototype orders them by its simulation's node index,
  which equals the relay. The production router must order them by the
  neighbour's Node ID, or the binding must supply the equivalent key.
- **A preference for the unmarked side at any cost.** Unmarked beats marked only
  at equal draining, priority, and cost: a strictly cheaper marked route still
  wins. Priority outranks cost, so an operator who wants the unmarked side
  preferred raises its priority.
- **Which relays announce.** Under `claimed` a relay offers a route to itself
  from its first claim on, for good, and whether a relay with no endpoint ever
  gets a claim across a scoped grant can depend on arrival order. So the set of
  route origins depends on history. The proof takes it as a link label fixed
  within an epoch. `sticky.rs`'s `whether_a_relay_announces_hangs_on_arrival_order`
  shows a relay that keeps a route to itself with no claim behind it.

## On paper

**Uniqueness.** `accelerated-fixed` shows the fixed point of the rule without
floors and named drains is a fixed point with them. Both rules converge, and a
convergent iteration started at a fixed point stays there, so each has exactly
that fixed point.

**The hypotheses hold.** A floor holds a session only once the node learned it
closed, from its own session ending or from a neighbour naming it. Sessions
between a pair of relays are numbered upward within a start, and with `epoch` a
later start orders above every session of an earlier one, so no live session
sits at or below a floor (`FloorsClosed`). A relay that sends GOAWAY sends it on
every session, and a node is named draining only by a neighbour that heard it
(`DrainNamed`).

**Advertising once per burst.** The library's schedules let a node act at any
subset of times on any earlier state of its neighbours. Reading a burst and then
advertising is one such schedule, and the iteration's function is unchanged, so
the fixed point is too.

**A function of the latest heard.** A node keeps the latest offer from each
neighbour for each origin node, delivered in order on one stream, and picks with
`candidates` and `order` over those alone. A withdrawal is the invalid route.
In-order delivery is a special case of the library's data-flow function.

**Dead hops and ended routes.** With `floor`, every route a dead-hop
withdrawal would end names a session the floor holds closed, so the floor
already refuses it (`the_floor_refuses_every_dead_hop`). A route a neighbour
ended stays invalid until that neighbour's next offer, which is the iteration's
own withdrawal.

**`claimed`.** A node with no claim offers no route to itself. A request names
an origin node it chose among the claims it knows, so no request names a node
with no claim, and refusing its trivial route removes nothing a request would
use. The flag only turns on (until a restart, which is a new node), so within an
epoch it is a fixed label.

**Claims after routes.** A claim's rank is the route to its announcer joined
with the claim's own drain, priority, and cost. Once routes settle, the claims'
iteration runs over a fixed route state and is itself an increasing algebra, so
it settles too.

**Loop-free trees at the fixed point.** At the fixed point a node's route is its
next hop's route extended by one link, so the path shortens by one at each next
hop and ends at the origin node, and it never names the node holding it. The
next hops toward one origin node therefore form a tree, and a request naming
that origin follows it. `sticky.rs` checks this at every settled state: each
route's path is its next hop's route with that hop added.

**The rule as it runs.** Sobrinho's liveness theorem for strictly monotone
algebras ("An algebraic theory of dynamic network routing", IEEE/ACM ToN 13(5),
2005, Theorem 6) does not cover it as stated. His model assumes a fixed total
preference among couplets of equal weight, and his proof's fourth case uses that
the couplet a node chose was preferred to every couplet it could choose then.
Staying on the next hop in use prefers by state instead. The argument here rests
only on the rank R = (draining, priority, cost) being strictly increasing: every
port costs at least 1 (`Router::connect` asserts it) and drain and priority
never improve. Order the finitely many values of R a path of at most 32 hops can
take. Suppose that from some time on, no node's selection of rank below r
changes. Once every message sent before then is delivered, every candidate of
rank r at a node extends a neighbour's settled selection of rank below r, so the
candidates of rank r at each node are fixed: filters read only the route, the
link, and what the node knows of closed sessions and draining nodes, which stops
changing once the epoch's news is delivered. A node whose best candidate has rank r picks among
those by mark, then the next hop in use, then length and neighbour; with the
candidates fixed it changes at most once more, and then stays. So selections of
rank at most r settle too, and by induction all do.

Which route a node settles on can differ, but not its rank or mark. A candidate
a path-dependent filter refuses at node i is one whose path names i, and then i
already holds that path's prefix at a strictly lower rank; or one past the hop
limit; or one through a floored session, and no settled path crosses one. So
every node settles on the rank and mark the fixed order gives it, except where a
neighbour's longer path meets the 32-hop limit, and only the next hop varies.
`sticky.rs` checks this: 150 random networks, through churn, each settled twice
from cold in two delivery orders, differ in next hop at 29 of 2,272 routes and
never in rank, mark, or reachability. With the mark below the next hop in use
the same sweep differed in the mark at 19, and
`an_equal_route_in_use_never_leaves_a_neighbour_without_one` shows the
neighbour that order left without a route.

## Moving the binding to the production router

The tests bind to the prototype, which the router quest
([router](/quest/m2/multicast/router.md)) replaces with `route/relay.rs`. When it
lands, point `algebra.rs` and `sticky.rs` at the router's equivalents of
`Offer::own`, `Offer::onward`, `Here::arrive`, `order`, and `candidates`,
keeping them one function each that the router itself calls. Instantiate
`Route` with the router's neighbour key in place of `Sim`'s `relay` and
`start`, or order neighbours by Node ID and change `Route`'s path order to
match. Then regenerate `Vectors.agda` and run `just test route-proof`.
