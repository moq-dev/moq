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

- Implement the rule the [epic](/quest/m3/cache-routing/README.md)'s item 14
  records ([moq-dev/moq#8732](https://github.com/moq-dev/moq/pull/8732)) by
  moving `decide` and `offer_from` out of `rs/moq-net/src/route/tests/cache.rs`
  into production code that the fixtures and sweep then drive, the way `Sim`
  drives `path::Router`: today they test the rule against itself and pass
  without touching the session. Add a check of the Goal's efficiency to
  `steady()`, copies held against a cold-routing baseline, since a rule that
  never offered a copy passes the sweep now. The rule covers the
  warm messages and SUBSCRIBE's `Warm` byte (which return to the wire with
  it, below), warm state derived from each front's demand, and the choice of
  what to pull. The long sweep already runs nightly from
  `test/route-sweep.sh`.
- Extend today's selection rather than adding a second selector.
  `route_order` in `rs/moq-net/src/model/origin.rs` already hashes equal
  routes per path, and a front already moves to a better route of its own
  instance and resplices (`rs/moq-net/src/model/front.rs`).
- A warm route names the origin and instance it carries, and a front moves
  only to a copy of its own instance.
- Bound the warm state a peer can make a relay hold by design, as
  announcements are bounded.
- Each pull sets SUBSCRIBE's `Warm` byte, which #8732 put in place of the
  `Serves` field; `js/net`, which forwards nothing, sends 0 (follow the
  route) and skips the warm types as unknown.
- Export upstream copies held, warm routes sent and received, and moves by
  cause through the relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; warm routes change only which neighbor a
  request goes to. If the rule's wording contradicts warm routes, bring a
  rewording to the maintainer.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)):
✅ lite-07 freezes without the warm wire
([warm routes on the wire](/quest/m2/warm-routes-wire.md)), so this quest
re-adds WARM_START, WARM_UPDATE, WARM_END, and SUBSCRIBE's `Warm` byte in the
then-current wip version, and first measures copies and relays crossed
against plain path routing.

Public API: report any relay configuration added for warm routes. Wire: the
warm messages and the `Warm` byte return in the then-current wip version,
with the draft's Warm Routes section.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the route layer driving lite-07 sessions since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023); its switch to path routes deletes the feasibility distances this rule once read
- [Warm routes on the wire](/quest/m2/warm-routes-wire.md) - may strip the warm wire from lite-07 or move this epic downstream before this starts
- [Transient cycles](/quest/m3/cache-routing/transient-cycles.md) - lowers the churn-time pull cycles the rule allows
