# [XL] Warm routes and per-broadcast parent selection

## Goal

Relays send warm routes as the
[epic](/quest/m3/cache-routing/README.md) decides. Each relay pulls every
demanded broadcast through its cheapest eligible neighbor, and moves an
existing copy when a cheaper eligible one appears or its warm parent
withdraws. A peer that never sends warm routes sees only cold routes and
behaves as it does today. Relays export how many upstream copies they hold
and how many moves warm routes caused.

## Plan

- Implement what [Prove cache-aware routing](/quest/m2/cache-routing-proof.md)
  settles for routes that propagate (the epic's item 13) and pass its
  fixtures and sweep: the warm message as the proof shapes it, warm state
  derived from each front's demand, and eligibility. Lite-07 today carries the
  one-hop warm routes and `Serves` that `rs/moq-net/src/route/tests/cache.rs`
  models, and `rs/moq-net/src/lite/` reads and ignores them; the proof
  rewrites both first. Once every scenario settles, run the long sweep
  nightly, which the merge gate skips as slow.
- Extend today's selection rather than adding a second selector.
  `route_order` in `rs/moq-net/src/model/origin.rs` already hashes equal
  routes per path, and a front already moves to a better route of its own
  instance and resplices (`rs/moq-net/src/model/front.rs`).
- A warm route names the origin and instance it carries, and a front moves
  only to a copy of its own instance.
- Bound the warm state a peer can make a relay hold by design, as
  announcements are bounded.
- Each pull states whom it serves, where Rust says unknown today: alone for
  a local reader or a client session, which forwards nothing. `js/net`
  already says alone and skips the warm types as unknown, so it needs
  nothing.
- Export upstream copies held, warm routes sent and received, and moves by
  cause through the relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; warm routes change only which neighbor a
  request goes to. If the rule's wording contradicts warm routes, bring a
  rewording to the maintainer.

Public API: report any relay configuration added for warm routes. Wire: a
change the implementation needs updates the draft's Warm Routes section in
the same PR, in lite-07 while it is unpublished and in the next version
after.

Noted 2026-11-12 (/quest-audit): this plan predates the path rule (epic Plan
item 8, [moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)). Its
one-hop warm routes and feasibility-distance eligibility are rewritten once
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) settles.

## Required

- [Prove cache-aware routing](/quest/m2/cache-routing-proof.md) - the rule for routes that propagate, its fixtures, and the wire this implements

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the route layer driving lite-07 sessions since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), whose feasibility distances eligibility reads
