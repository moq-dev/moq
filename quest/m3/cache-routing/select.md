# [XL] Warm routes and per-broadcast parent selection

## Goal

Relays send warm routes to their direct neighbors as the
[epic](/quest/m3/cache-routing/README.md) decides. Each relay pulls every
demanded broadcast through its cheapest eligible neighbor, and moves an
existing copy when a cheaper eligible one appears or its warm parent
withdraws. A peer that never sends warm routes sees only cold routes and
behaves as it does today. Relays export how many upstream copies they hold
and how many moves warm routes caused.

## Plan

- Implement the rules the epic's Plan records as proven, which
  `rs/moq-net/src/route/tests/cache.rs` models over the real router: the
  Warm Routes SETUP extension (warm routes on the announce stream, and the
  relay each pull serves on SUBSCRIBE), warm state per neighbor derived from
  each front's demand, and eligibility through `Router::eligible` with the
  rendezvous hash ordering tied routes. Port that file's fixtures and sweep
  to the implementation, or drive it from them.
- First make `sweep_cache_routing_long` settle: 4 of its 3000 scenarios
  never do under the epic's item 10, and its `#[ignore]` comes off once they
  do. Land after the overtaking-echo fix that
  [moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447) found, which
  item 8's argument needs.
- Extend today's selection rather than adding a second selector.
  `route_order` in `rs/moq-net/src/model/origin.rs` already hashes equal
  routes per path, and a front already moves to a better route of its own
  instance and resplices (`rs/moq-net/src/model/front.rs`).
- A warm route names the origin and instance it carries, and a front moves
  only to a copy of its own instance.
- Bound the warm state a peer can make a relay hold by design, as
  announcements are bounded.
- Only relay sessions that negotiated warm routes exchange them. A browser
  never parents and never negotiates them, so `js/net` needs nothing.
- Export upstream copies held, warm routes sent and received, and moves by
  cause through the relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; warm routes change only which neighbor a
  request goes to. If the rule's wording contradicts warm routes, bring a
  rewording to the maintainer.

Public API: report any relay configuration added for warm routes. Wire: the
Warm Routes extension as the epic's Plan designs it, with its draft section in
the same PR.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the route layer driving lite-07 sessions since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), whose feasibility distances eligibility reads
