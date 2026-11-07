# [L] Route session integration: bounded state

## Goal

The route layer drives every lite-07 session
([#8023](https://github.com/moq-dev/moq/pull/8023)): one `route::Node` per
origin, Route IDs on the wire, requests pinned to the first hop's origin node,
lite-06 translated at the relay. What remains: the state a node keeps per
stream it answers is bounded by the origins that stream reads, not the whole
node table, and the public API drops what the route layer replaced.

## Plan

- Scope route state to interested origins, not the CDN's whole node table.
  Today the router advertises every route to every watching stream. The
  epic's handoff contract now has a sender send a route on a stream only
  while it meets the stream's interest, with a floor certifying the last
  withdrawal before the route ends (`routes/README.md` Plan item 15, decided
  2026-11-06): start the route before its first claim in or covering the
  interest, and end it after the floor that certifies its last such claim's
  withdrawal. Update `a_disjoint_interest_is_answered_with_nothing` to match.
  What a peer makes the router hold is already capped: each route and claim
  it sends takes one of the session's announce slots
  (`a_lite07_claim_past_the_cap_closes_the_session`). What this bounds is
  the state this node keeps per stream it answers.
- Present incrementally. `View::present` walks every claim of a route per
  change and per interest (`Router::advertise`), so a large origin costs
  linear work per update. Do it with the scoping change, which reshapes the
  same per-stream presentation.
- Public API: the hop chains leave it before the cut. Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests):
  ✅ now, behind a private chain type that lite-06 and the moq-transport
  cluster extension keep filling (rejected: when lite-06 retires, which ships
  them in the cut and breaks them later; only the `Cost` constants now). The
  sweep found no reader outside this repository.
  - `Hops`, `RouteInfo::hops`, `RouteInfo::is_anonymous`, and
    `Cost::{MAX, DRAIN}` become `pub(crate)`; `Cost::{new, value}` stay.
    JS `RouteInfo.hops`, moq-ffi's `MoqRouteInfo` hops, and moq-c's route
    info drop their mirrors.
  - A drained route is the lowest preference, never a saturated metric
    ("drain is a preference state", in the
    [epic](/quest/m2/cluster-routing/README.md)), so sessions stop stamping
    `Cost::DRAIN` on GOAWAY, including on moq-transport.
  - An origin has one identity on both wires, seeded from `Config.hop`. Today
    the lite-07 route node is `route::Node::random()`
    (`rs/moq-net/src/model/origin.rs`) and lite-06 derives a different hop
    from it (`route_hop`).

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract
- [Route wakes](/quest/m4/route-wakes.md) - index work follows the settled source-selection model
