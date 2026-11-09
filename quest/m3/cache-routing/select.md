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

- Implement what [Prove cache-aware routing](/quest/m3/cache-routing/proof.md)
  settles and pass its fixtures: the warm message where it placed it, warm
  state per neighbor derived from each front's demand, and eligibility over
  the router's feasibility distances.
- Extend today's selection rather than adding a second selector.
  `route_order` in `rs/moq-net/src/model/origin.rs` already hashes equal
  routes per path, and a front already moves to a better route of its own
  instance and resplices (`rs/moq-net/src/model/front.rs`).
- A warm route names the origin and instance it carries, and a front moves
  only to a copy of its own instance.
- Bound the warm state a peer can make a relay hold by design, as
  announcements are bounded.
- Only relay sessions that negotiated warm routes exchange them. A browser
  never parents, so `js/net` needs at most to skip the message.
- Export upstream copies held, warm routes sent and received, and moves by
  cause through the relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; warm routes change only which neighbor a
  request goes to. If the rule's wording contradicts warm routes, bring a
  rewording to the maintainer.

Public API: report any relay configuration added for warm routes. Wire: the
warm message as the proof placed it, with its draft section in the same PR.

## Required

- [Prove cache-aware routing](/quest/m3/cache-routing/proof.md) - the fixtures and wire placement this implements

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the route layer driving lite-07 sessions since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), whose feasibility distances eligibility reads
