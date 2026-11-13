# [L] Prove cache-aware routing

## Goal

Sweeps over the route layer show that the [epic](/quest/m3/cache-routing/README.md)'s
warm routes, which propagate per broadcast, stay loop-free at every step,
under stale, reordered, and in-flight messages. Once demand and topology stop
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
one-hop warm route first decided (epic items 8 to 12), and lite-07 still
carries its wire (WARM_START, WARM_UPDATE, WARM_END, and `Serves` on
SUBSCRIBE), which Rust reads and ignores.

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
- **Truthful offers.** A relay offers every neighbour that reads copies its
  current copy, even one whose path names that neighbour (poisoned reverse,
  not split horizon), and pulling through a neighbour means its copy when it
  offers one, its route only when it does not. With split horizon a request
  that follows a route reaches a relay that forwards it into a copy leading
  back, a cycle that outlives quiescence (scenario 1563 of the long sweep).
  A copy whose path names a neighbour the relay serves is unusable, and a
  relay serving a neighbour that reads no copies pulls through its next hop
  alone, since that neighbour's pulls follow the route blind.
- **GOAWAY** keeps new pulls off the draining neighbour's copy, rather than
  ranking it last ahead of cost, which let two relays each prefer the
  other's propagation of the draining carrier's copy (scenario 149 without
  holds).
- **A pull on another relay's behalf offers nothing new**, so `Serves` and
  its roots are not needed: a relay with no reader offers only a copy it
  joined, at its cost.

Measured (`sweep_cache_routing`, 100 scenarios each with and without held
links): every scenario settles on the steady state, an idle broadcast sends
no warm message, and about 3.3 warm messages go out per reader change.
`copies.rs` is folded into `cache.rs`, and `Router::eligible` is gone with
the Babel-based model.

Not loop-free at every step: a broadcast's pulls form a cycle after 0.5% of
churn steps with held links and 0.65% without (11 and 15 of 100 scenarios),
nearly always with a reader behind it, each ending when the withdrawal
crossing its link is delivered, at most 32 deliveries later in the sweep's
random order, and never at quiescence. The fixtures record where it happens:
six relays of a full LAN mesh gaining readers at once cross offers for 5
steps. A path cannot do better: two relays acting on each other's stale
offers both join in the same instant, which only a distance per broadcast
with a reset (a seqno or an echo, as item 27 removed from routes) prevents.

Remaining:

- `sweep_cache_routing_long` (3000 scenarios each way) still has one
  scenario that never settles, 246 with held links: one carrier's copy
  circulates round a ring of five, each relay's choice flipping as the
  copies its downstream names come and go. Settle it, then run the long
  sweep nightly from `test/route-sweep.sh` and drop its line from
  `sh/gh/test-inventory.allow`.
- Then the wire, in the same PR: WARM_START carries the copy's Hops (as
  ROUTE_START does), its cost, and its Carrier, WARM_UPDATE replaces them,
  and a warm route may name the receiver; `Serves` leaves SUBSCRIBE and
  SUBSCRIBE_UPDATE. Update the draft's Warm Routes section, the Rust codec
  (`rs/moq-net/src/lite/announce.rs`, `subscribe.rs`), and `js/net`, and the
  [epic](/quest/m3/cache-routing/README.md)'s items 2, 3, 9, and 10, which
  these rules replace.

## Waiting

- Maintainer: the Goal asks for loop-free at every step, which paths alone
  cannot give (see above). Options: accept a cycle that lasts until the
  crossing withdrawal arrives, as routes item 27 accepted for selected
  routes, and reword the Goal to "loop-free once the offers in flight are
  delivered"; or add a per-broadcast distance, the carrier's rank then the
  cost, with a reset by echo or timer, which brings back what item 27
  removed. Recommended: accept, since a cycle here only delays a reader by
  one crossing and the distance costs a message per reset. Check: the Goal
  reads one way or the other.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the path rule this extends to warm routes
