# [L] Prove cache-aware routing

## Goal

Sweeps over the route layer show that the [epic](/quest/m3/cache-routing/README.md)'s
warm routes, which propagate per broadcast, stay loop-free once the offers in
flight are delivered, under stale, reordered, and in-flight messages. Once demand and topology stop
changing, every demanded broadcast is delivered, parent choices stop
changing, and a relay carries a broadcast only when it has a local reader,
feeds two or more downstream sessions, or is its consumers' cheapest route
given the copies the others hold, as the epic's Goal states. Lite-07's warm
message carries the proven rule.

## Plan

Decided 2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet
plan): ✅ pull the proof into m2, ahead of the lite-07 cut, so a warm
message that ships in lite-07 is the proven one (rejected: a design-only
quest in m2 that sizes the message with no proof; leaving this in m3 and
taking an additive SETUP extension if it lands after the cut). This reverses
the 2026-11-08 choice of m3 for the proof alone;
[the implementation](/quest/m3/cache-routing/select.md) stays in m3.

Reopened 2026-11-11 (maintainer, /quest-plan on the route redesign, epic
Plan item 13): a warm route propagates, and the rule that keeps it free of
loops and stale copies is the route's path
([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27,
picked 2026-11-11 on
[moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)), which also
removes the seqno, ROUTE_REQUEST, and the ROUTE_END echo.

[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) proved the
one-hop warm route first decided (epic items 8 to 12).

Found 2026-11-12 ([moq-dev/moq#8615](https://github.com/moq-dev/moq/pull/8615)),
rewriting `rs/moq-net/src/route/tests/cache.rs` on the path prototype in
`paths.rs` (Rules item 27 picked: causes, holds, a 4-hop doubt timer), with
demand, subscriptions on their own streams, lingers, GOAWAY, legacy peers,
and instances. A path alone does not settle copies that propagate; each rule
below was forced by a sweep scenario that otherwise never settles or loops
for good:

- **Monotone copy costs.** Only a relay with a reader whose pull follows the
  publisher's route offers its copy at no cost; a relay that joined a copy
  offers it at what its own pull costs. Pricing every reader's copy at 0
  made a dispute wheel round a ring of five, two consistent states that a
  lockstep schedule flips between forever (scenario 77). Joining a copy
  still costs only the links from where it left the publisher's route.
- **Carrier rank.** Every copy names its carrier, the relay with a reader
  where it left the publisher's route. A relay on a copy, its own included,
  moves onto another, its next hop's included, only if that copy's carrier
  ranks below by the Rendezvous Hash. A rank per pair of neighbours, or only
  between readers, left longer crossing cycles trading places (scenarios 139
  and 72).
- **A relay off a copy is bound by the copies it feeds.** A relay pulling
  along its route with no reader of its own, or with no demand at all, hands
  whatever it joins to every neighbour that pulls through it or whose route's
  next hop it is, since a next hop's copy is no choice. It joins a copy other
  than its next hop's only if that copy's carrier ranks at or below the
  carrier each such neighbour offers back along this relay's route, and none
  while a neighbour that pulls through it has offered none. Free, a relay
  with no reader joined a higher-ranked copy and relabelled the carrier
  behind it, and a copy circulated round a ring of five (scenario 246 of the
  long sweep); an offer back along another path is an echo of the relay's
  own old copy, which kept two relays crossing every four rounds.
- **Truthful offers.** A relay offers every neighbour that reads copies its
  current copy, even one whose path names that neighbour (poisoned reverse,
  not split horizon), and pulling through a neighbour means its copy when it
  offers one, its route only when it does not. With split horizon a request
  that follows a route reaches a relay that forwards it into a copy leading
  back, a cycle that outlives quiescence (scenario 1563 of the long sweep).
  A copy whose path names a neighbour the relay serves is unusable. A relay
  serving a legacy neighbour may still join a copy: the path check keeps out
  one through that neighbour, and refusing every copy while its subscription
  came and went kept a ring flipping.
- **GOAWAY** moves every pull off a draining neighbour's copy other than
  the next hop's, rather than ranking it last ahead of cost, which let two
  relays each prefer the other's propagation of the draining carrier's copy
  (scenario 149 without holds).
- **A pull on another relay's behalf offers nothing new**, so `Serves` and
  its roots are not needed: a relay with no reader offers only a copy it
  joined, at its cost.

Decided 2026-11-13 (maintainer, /quest-run): ✅ accept a cycle that lasts
until the crossing withdrawal is delivered, as routes item 27 accepted for
selected routes, and word the Goal "loop-free once the offers in flight are
delivered" (rejected: a per-broadcast distance, the carrier's rank then the
cost, with a reset by echo or timer, which brings back the seqno and echo
item 27 removed). Two relays acting on each other's stale offers both join
in the same instant, which only such a distance prevents.

Measured (`sweep_cache_routing_long`, 3000 scenarios each with and without
held links, the gate's seed): every scenario settles on the steady state, an
idle broadcast sends no warm message, and about 3.2 warm messages go out per
reader change. A broadcast's pulls form a cycle after 0.39% of churn steps with
held links and 0.50% without, each ending when the withdrawal crossing its
link is delivered, at most 53 deliveries later in the sweep's random order.
Each subscription is a stream of its own, so a replacement pull can overtake
the cancel of the stream it replaces, and the steady state holds one stream
per neighbour fed. The
LAN fixture keeps one uplink copy per broadcast in a full mesh of 6, and 15
copies for 8 broadcasts in a ring of 6. `settle` lets a hop of delay pass every
64 rounds of deliveries, so a doubt lifts during a long exchange as it would.

Lite-07 carries the rule: WARM_START and WARM_UPDATE carry the copy's Warm
Cost, its carrier's position in its Hops, and the Hops, and `Serves` left
SUBSCRIBE and SUBSCRIBE_UPDATE
([moq-dev/moq#8615](https://github.com/moq-dev/moq/pull/8615)).

Remaining:

- Under other seeds the long sweep still finds scenarios that never settle,
  2 in 48,000 across 8 seeds (the rules first proven here failed every seed
  within 1,200 scenarios): `MOQ_ROUTE_SEED=305419896` scenarios 196 and
  1391, both without held links. Each rule above closed one such class and
  exposed a rarer one, and binding a
  relay by the copy a neighbour routed through it sits on now, wherever it
  came from, made a third seed fail, so the next step is an argument that
  the rules converge, such as a TLA+ model of the copy choice alongside
  `verify-route`'s, rather than another rule per scenario. Replay one with
  `MOQ_ROUTE_SEED=<seed> cargo test -p moq-net --release --lib
  route::tests::cache::sweep_cache_routing_long -- --ignored`.
- Once fresh seeds settle, run the long sweep nightly from
  `test/route-sweep.sh` with a fresh `MOQ_ROUTE_SEED`, and drop its line from
  `sh/gh/test-inventory.allow`.
- The [epic](/quest/m3/cache-routing/README.md)'s items 2, 3, 9, and 10
  describe the one-hop warm route these rules replace; its item 14 records
  the replacement.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the path rule this extends to warm routes
