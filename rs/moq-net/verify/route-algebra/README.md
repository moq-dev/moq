# The route rule's convergence proof

The route rule `route::relay::Router` runs (`rs/moq-net/src/route/relay.rs`), and
the claims that ride on it, stated as increasing path algebras in Agda, with
Daggitt and Griffin's Theorem 5 applied to them
([arXiv 2106.01184](https://arxiv.org/abs/2106.01184), library
[MatthewDaggitt/agda-routing](https://github.com/MatthewDaggitt/agda-routing)):
on every network, from every state, with messages delayed, reordered, lost, and
repeated, the asynchronous iteration reaches the network's unique fixed point
once the network holds still long enough.

"Long enough" is a premise on progress, not only on time (the paper's
Definitions 14 to 17): while the network holds still, every node must keep
acting, and every node must keep hearing from each neighbour something sent
after any given moment, a fixed number of times over. Loss is tolerated only
within that. A link that loses everything for good never converges, though
nothing about the network changed. A session here is an ordered, reliable
stream, so it delivers or it closes, and a close is a change to the network.

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
| `Claim.agda` | Claims across trust boundaries: the same algebra at the grain of the relays that hand a claim on. |
| `Named.agda` | `Route` with nodes named as the binding names a `NodeId`. |
| `Vectors.agda` | Route results the router computed, each a `refl` (generated). |
| `ClaimVectors.agda` | Claim results the router computed, each a `refl` (generated). |
| `Accelerate.agda` | The floor and the named drains leave the rule's fixed point unchanged. |

## The algebra

A route is invalid or carries what leads its rank (nothing for a route, a
claim's precedence), whether it is draining, a penalty (127 minus the lowest
priority on its path), a cost, whether a marked link is on its path, its path,
and whatever else a level carries (`extra`). Routes rank by the lead, then
draining, then penalty, then cost, then unmarked before marked, then the shorter
path, then a fixed order on the path and `extra` that each level gives. A node
keeps the lower of two routes. Crossing a link refuses a route whose path does
not start at the link's far end or already names its near end, and whatever the
link's filter refuses; otherwise it can only raise the lead, make the route
draining, raise its penalty, add to its cost, mark it, prepend the link, and set
`extra`. That is an increasing path algebra, which is all Theorem 5 asks.

**Routes.** A node is a Node ID: a relay, which names one run of its process, or
its marked identity, which announces what the node took over a marked link. A
restarted relay is a new relay. Among equal ranks, marks, and lengths, routes order by the neighbour's
Node ID (relay, unmarked first), as `order` does. The link from i to its
neighbour j carries: whether it is up, i's port cost and penalty, whether j sent
GOAWAY, the nodes i knows draining, i's session floors, whether i or j marked
the link, whether j announces a claim, and i's session number. It refuses a
route when the link is down, when the route is j's route to itself and j
announces no claim, when the route is marked and j marked the link, when the
path would grow past 32 hops, when the cost would pass the metric, or when the
path names a session i's floors hold
closed. A route that names a node known draining ranks as draining. A node's
marked identity hangs off the node by one free, marked link at session 0, so its
route there is the node's marked route, `[marked identity, node]`, whose marked
hop counts toward the 32.

**Claims.** The destination is a broadcast: one Identity at one path. A claim
filed at a prefix of the path reaches it lowered, its Above counted from the
path, so the claims of one prefix tie on precedence wherever each is filed and
rank as one tier, as `Router::tier` gathers them and the draft's Resolution
ranks them. A claim
leads with its precedence (a claim exactly at its path before a broader one shown
there, then the newer run) and orders, among equal ranks, marks, and lengths, by
its claimant's Node ID and then the key its holder gives the neighbour that gave
it, as `claim_order` and `restate` do. A step is the publisher's own, which sets
the run; an adoption from an endpoint or across a scoped grant
(`Claim::exported` at the giver, then `Claim::adopted`), which prices and marks
the claim over the link, lowers it where it is shown below its own path, files it
under the adopter or its marked identity, and refuses one the giver marked the
link against or whose path reaches the longest chain; or a domain step
(`Claim::over`), which joins an announcer's claim with the settled route there.
Every step into a node refuses a claim below the best precedence the node knows
of, reachable or not, so the rank chooses only among claims of that precedence,
as `Router::tier` and `closest` do. A claim's path names each relay that handed it on, then the
publisher and the broadcast: Rust's chain backward, with the holder in front
where it holds another node's claim.

## Coverage

**Mechanized** (Agda, `--safe`):

- The generic algebra is an increasing path algebra, so it converges, and the
  synchronous iteration within n² rounds (`Increasing.convergent`,
  `converges-n²`). This covers: selection is one total order with a fixed
  tie-break; crossing a session never makes a route preferred; a path naming
  its receiver is invalid and a path grows by exactly the hop it crossed; each
  filter maps a route to invalid; the named drain keeps one order.
- The route identity with every filter: claimed, the mark's export filter, the
  hop limit, the metric, the floor, and the named drain (`Route`).
- The claim identity with precedence leading, the mark and its export filter,
  the chain's limit, the best known precedence, and the claimant and giver order
  (`Claim`). Echoes never beat the claims they echo because every step is
  increasing.
- The floor and the named drains only make the rule arrive sooner: the fixed
  point of the rule without them is a fixed point with them, given that no floor
  covers a live session (`FloorsClosed`) and that a node is named draining only
  once it sent GOAWAY on every live link (`DrainNamed`)
  (`Accelerate.accelerated-fixed`).

**Bound to the router by tests** (`rs/moq-net/src/route/tests/algebra.rs`): the
router's own functions, which it calls itself, meet the axioms on random links,
routes, and claims, and 649 of their results are written into the two vector
files, where Agda must compute the same results by `refl`: for routes,
`Offer::own`, `Offer::onward`, `Here::arrive`, and `order` (2 of the receiver's
marked route, 200
single steps, and 127 choices between two routes); for claims, `Claim::over`,
`Claim::exported`, `Claim::adopted`, `precedence`, and `claim_order` (200 single
steps and 120 choices between two candidates). Two thirds of the choices tie up
to the neighbour, the claimant, or the giver, and half of those also change the
mark or lengthen the path, so each tie-break decides some. A Rust test fails
when a file is stale (`ROUTE_VECTORS=write` regenerates both), and a changed
Agda definition fails the nightly check. Planting any of these in the router is
caught by a vector: an unmarked marked route, a marked route sent over a marked
link, the floor ignored,
the hop limit off by one, the named drain ignored, the priority not lowered, the
mark, the neighbour's Node ID, or the path length left out of `order`, the mark
or the claimant left out of `claim_order`, a join that keeps the claim's cost or
drops the route's mark, a claim exported over a marked link, an adopter taking a
chain that names it, keeping the priority, ignoring the port's mark, or never
naming itself, and an older run ranked as the newer. The exporter's own chain
limit is not, since the adopter's is stricter. This is a sample: no test shows
the Rust functions equal the Agda definitions on every input.

**Argued on paper** (below): the fixed point is unique with and without the
accelerators; the two hypotheses hold in the router; advertising once per
burst changes no fixed point; what a node advertises is a function of the
latest it heard; the dead-hop and ended withdrawals remove only what the floor
does; `claimed` removes only routes no request names; claims settle once routes
do; the tree is loop-free at the fixed point; and the rule as it runs, with the
next hop in use, still settles.

**Only tested**: the rule as it runs settles, on the rank and mark the fixed
order gives (`sticky.rs`, on `relay::Router`); the floor refuses every route the
dead-hop withdrawal ends (`algebra.rs`, `the_floor_refuses_every_dead_hop`); a
claim's path in Rust grows by exactly the relay that took it
(`a_claim_step_never_improves_a_claim`).

## What this does not give

- **The rule as it runs.** The theorem is for ties broken by one fixed order.
  The router puts the next hop in use ahead of the shorter path and the
  neighbour, so which of two equal routes a node settles on depends on which
  came first, and the stable state is not unique. The argument that it still
  settles is on paper and tested, not mechanized (below).
- **The router's state machine.** The binding runs the functions that rank,
  extend, and refuse one route or claim, not the state around them: which offers
  a node keeps, the floors and drains it learns, the revisions that order a
  node's claims, the readers' debounce, and what `route` and `adopt` refuse as
  malformed, which closes the session. That a node feeds those functions the
  latest offer from each neighbour is argued on paper (below).
- **Loop freedom at every instant.** Only the fixed point is loop-free.
  [Requests](https://github.com/moq-dev/moq/pull/8881) name their origin node
  and fail closed instead.
- **A cost that depends on who subscribed.** Warmth is not in the algebra;
  [warmth](https://github.com/moq-dev/moq/pull/8903) argues it separately.
- **Liveness under churn that never stops.** Convergence needs each epoch to
  last long enough.
- **The chain where a claim stays inside a domain.** The claim identity's path
  names every relay that took the claim, and each refuses a claim naming it.
  Rust's chain names the relays a claim entered or left a routing domain by:
  the announcing relay, then each grant's exporter and adopter, a relay that
  adopts across one grant and exports across the next once, which is that path
  without the steps within a domain. A relay across a grant refuses a claim
  whose chain, with it appended, names a relay twice, so it refuses what the
  algebra refuses at it as adopter and at its exporter. Inside a domain no node
  checks the chain: a claim that left a domain and came back by a third relay is
  announced there, as the algebra allows, and a node of that domain the chain
  names, which the algebra has refuse it, still ranks it below the claim it
  echoes, and the relay across the next grant it would leave by refuses it. The
  binding samples no such step.
- **Which claims a node knows.** The best precedence a node knows of, reachable
  or not, is a label on its links, fixed within an epoch, like `claimed`. It is
  not: a claim no route reaches stays known while any neighbour still offers its
  announcer, and a claim told unavailable across a grant is taken only as news of
  one the giver gave before. An unavailable claim is otherwise outside the
  algebra, which has it invalid.
- **Two sessions to one neighbour.** The algebra has one link per pair of nodes.
  The router can hold two sessions to one Node ID, and breaks a tie between them
  by the session's key, after everything the algebra orders by. Their floor is
  one per pair, which is why a relay never names a session that closes while an
  older one to that relay lives (`a_second_session_keeps_the_route`).
- **A saturated claim cost.** A claim's cost saturates at the metric, so a step
  past it adds nothing, where the algebra's always adds. The binding samples
  costs far below it.
- **An application's claims.** A router that does not forward names itself in no
  chain, so its own claims' paths are a step shorter than a relay's. The binding
  samples relays only.
- **A preference for the unmarked side at any cost.** Unmarked beats marked only
  at equal draining, priority, and cost: a strictly cheaper marked route or claim
  still wins. Priority outranks cost, so an operator who wants the unmarked side
  preferred raises its priority.
- **Which relays announce.** A relay offers a route to itself from its first
  claim on, for good, and whether a relay with no endpoint ever gets a claim
  across a scoped grant can depend on arrival order. So the set of route origins
  depends on history. The proof takes it as a link label fixed within an epoch.
  `sticky.rs`'s `whether_a_relay_announces_hangs_on_arrival_order` shows a relay
  that keeps a route to itself with no claim behind it.

## On paper

**Uniqueness.** `accelerated-fixed` shows the fixed point of the rule without
floors and named drains is a fixed point with them. Both rules converge, and a
convergent iteration started at a fixed point stays there, so each has exactly
that fixed point.

**The hypotheses hold.** A floor holds a session only once the node learned it
closed, from a neighbour naming it. A relay names its own session only if none it
numbered lower to the same relay is live when it closes: a newer session that
closes first, while an older one lives, is never named, and its routes go by
replacement, as any route change does. A relay numbers its sessions upward, and
a restarted relay is a new relay with floors of its own, so no live session sits
at or below a floor (`FloorsClosed`). A node also forgets
floors, which only drops an accelerator. A relay that sends GOAWAY sends it on
every session, and a node is named draining only by a neighbour that heard it
(`DrainNamed`).

**Advertising once per burst.** The library's schedules let a node act at any
subset of times on any earlier state of its neighbours. Reading a burst and then
advertising is one such schedule, and the iteration's function is unchanged, so
the fixed point is too.

**A function of the latest heard.** A node keeps the latest offer from each
neighbour for each origin node, delivered in order on one stream, and picks with
`Router::usable` and `order` over those alone. A withdrawal is the invalid route.
In-order delivery is a special case of the library's data-flow function.

**Dead hops and ended routes.** Every route a dead-hop withdrawal would end names a session the floor holds closed, so the floor
already refuses it (`the_floor_refuses_every_dead_hop`). A route a neighbour
ended stays invalid until that neighbour's next offer, which is the iteration's
own withdrawal.

**`claimed`.** A node with no claim offers no route to itself. A request names
an origin node it chose among the claims it knows, so no request names a node
with no claim, and refusing its trivial route removes nothing a request would
use. The flag only turns on (until a restart, which is a new node), so within an
epoch it is a fixed label.

**Claims after routes.** A claim's rank is the route to its announcer joined
with the claim's own drain, priority, and cost (`Claim::over`). Once routes
settle, the claims' iteration runs over a fixed route state and is itself an
increasing algebra (`Claim`), so it settles too.

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
port to a forwarding node costs at least 1 (`Router::connect` refuses a free one) and drain and priority
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
from cold in two delivery orders, differ in next hop at 29 of 2,282 routes and
never in rank, mark, or reachability. With the mark below the next hop in use
the same sweep differed in the mark at 20, and
`an_equal_route_in_use_never_leaves_a_neighbour_without_one` shows the
neighbour that order left without a route.
