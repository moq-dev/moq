# [L] Warm pulls move for price and still settle

## Goal

The warmth simulator has a rule under which a pull moves to a cheaper warm
copy that turns up later, and a model check shows the rule settles: once
readers, links, and publishers stop changing, every pull stops moving and
every routed reader is fed, under every order of delivery at a small bound.
[The selection quest](/quest/m3/cache-routing/select.md) then names that rule
beside settled pulls, with what each costs and what it would add to the wire.

If no candidate holds up, the PR says how each one failed, and the model
check runs on settled pulls instead, with its fallback made part of the
rule if the check needs it.

This is a spike: no production code and no wire change.

## Plan

Decided 2026-11-16 (maintainer, in the coordinating session, after asking
what warmth would cost before the cut): spike warmth now. The coordinator
read that as this quest, run beside [the switch](/quest/m2/multicast/switch.md).
Whether any warm wire rides lite-07 is the maintainer's to decide on its
result, so report the wire it would need and do not add it.

Why a spike: [moq-dev/moq#8903](https://github.com/moq-dev/moq/pull/8903)
measured two rules and neither is the one to ship before the cut.

- Settled pulls settle in every case swept, but a pull never moves to a pop
  that turns warm later. On an unpeered CDN the first cut sends no warm
  message, so it is path joins with extra parts; on the pops shape it costs
  71 against path joins' 69. Its settling is argued, not shown.
- The price-moving pick does move (61 on pops, drones +9.4% over the
  cheapest against +16.2%), and never settles churned random mesh 503: a
  relay's own demand comes back to it as a cheaper price two hops away.

Every fence tried so far kept one shape of that reflection out. Look for a
rule where a relay's choice cannot depend on its own demand by construction.

**First candidate: order the readers.** The coordinator's, untested; attack
it before building on it.

- A relay with a reader of its own for an Instance has a key for it: its
  route cost to the Instance, then its Node ID. Also measure a hash of Node
  ID and Instance, which does not change with routes.
- A pull carries the lowest key among the readers it feeds: the relay's own,
  and each downstream pull's. Adverts and subscribe answers carry it.
- A relay takes a warm copy only from a neighbour whose key is below its
  own. Such a copy is fed whatever this relay does, so it costs the link
  alone. The relay takes the cheapest of those copies and its route, and
  chooses again whenever that set or its own key changes.
- The claim to test: the tree is then the one greedy joins build when
  readers arrive in key order, whatever order they really arrived in. A
  reader's path depends only on readers with lower keys, so it settles by
  induction on the key, and each step is a search for the nearest of a
  fixed set, the shape [the route proof](/rs/moq-net/verify/route-algebra/README.md)
  covers. Imase and Waxman's O(log k) bound holds for any arrival order, and
  here it would hold after departures too, since the tree is rebuilt.
- What it should deliver: the pop that turns warm later draws the pulls
  behind it; two drones that start at once merge, the higher key moving (the
  maintainer suggested a hash tie-break where one drone reneges); case 503
  settles.
- Where it may break: a relay that only forwards takes the key of the pull
  through it, and its key drops when a lower one arrives, so it must choose
  again and may move the higher keyed pulls it already feeds. A price that
  still reads demand above the first hop, as `O(P→R) + warm(P, R)` does,
  would bring the reflection back. Stale adverts and keys in flight.
- What it costs, to measure: moves and unfed reader-hops per reader change,
  an advert update when a key changes without the demand starting or
  ending, and one key per pull and per heard advert.

**Other candidates**, if the first fails or to beat it:

- Price each copy at what the pull adds to the tree. The tree's total cost
  is then a potential: a move that lowers a relay's price lowers the tree's
  cost by as much, provided the price is exact and no two moves overlap. The
  pick failed both. Exact needs the asker's demand left out at every hop,
  which naming sole demand tried and still left 1 of 2,000 unsettled;
  moves that do not overlap need a lock or an order.
- Count only relays with a reader of their own as warm, which no relay's
  choice changes, and order by route cost so two such relays never adopt
  each other. It loses coalescing at relays that only forward, which is
  every core of a CDN.

Work in `rs/moq-net/src/route/tests/warm.rs` and `warmth.rs`, as new arms
beside `advertise_not_down+hops+stay+reroute` and the pick, so every table
in [select](/quest/m3/cache-routing/select.md) gains a column.

Model the chosen rule in TLA+ under `rs/moq-net/verify/`, as the other
models there are laid out, with its nightly lane and skill:

- A few relays, readers starting and stopping, links failing and returning,
  adverts and keys delivered in any order and stale.
- Settling is checked from when the outside events stop, not from when the
  queues drain: a retry loop keeps the queues full.
- Both properties: the pulls go quiet, and every reader the route layer
  routes is then fed, two relays waiting on each other included.
- Show the check has teeth: the price-moving pick, or the rule with its
  order removed, must fail it.

This takes over the model check [select](/quest/m3/cache-routing/select.md)
had asked of the port; select now ports the rule this model checks.

Report in the PR:

- Each candidate tried and how it failed or held, with the case.
- The tables of [moq-dev/moq#8903](https://github.com/moq-dev/moq/pull/8903)
  with the new column: trees over the cheapest, unsettled, longest cycle,
  unfed reader-hops, warm messages, pops, drones, tiers, growth per link
  kind.
- The wire and relay state the rule would add over the first cut, and what
  a peer that does not send it sees.
- A recommendation between it and settled pulls for
  [select](/quest/m3/cache-routing/select.md), as a decision under
  `## Waiting` for the maintainer.

## Related

- [Selection](/quest/m3/cache-routing/select.md) - ports the rule this picks
