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
  Today the router advertises every route to every watching stream, because
  the epic's handoff contract (`routes/README.md`, "a sender that certifies
  anything for a route answers every stream") has a sender answer even a
  stream whose interest none of the route's regions meets. Scoping needs
  that sentence amended (see Waiting).
  What a peer makes the router hold is already capped: each route and claim
  it sends takes one of the session's announce slots
  (`a_lite07_claim_past_the_cap_closes_the_session`). What this bounds is
  the state this node keeps per stream it answers.
- Present incrementally. `View::present` walks every claim of a route per
  change and per interest (`Router::advertise`), so a large origin costs
  linear work per update. Do it with the scoping change, which reshapes the
  same per-stream presentation.
- Public API: `Hops`, `RouteInfo::hops`, and the saturating `Cost` stay while
  lite-06 and the moq-transport cluster extension still carry hop chains.
  Remove them, or hide them behind those wires, when the rollout window
  closes (see Waiting).

## Waiting

- Maintainer: amend the handoff contract's "a sender that certifies anything
  for a route answers every stream" (`routes/README.md`), or keep it. It is a
  decided contract, so an agent does not change it. Options: **send a route
  only while it meets the stream's interest (recommended)**: a route starts on
  a stream before its first claim in or covering the interest, and when its
  last such claim goes it sends a floor certifying the withdrawal, then ends
  the route, so withdrawals still END and per-stream state is bounded by the
  origins a peer reads, at a ROUTE_END round trip when an origin leaves an
  interest; or **keep answering every stream** and bound only by per-peer
  caps, simpler but carrying the whole node table on every stream. Done when
  the README says which.
- Maintainer: decide when `Hops`, `RouteInfo::hops`, and the saturating
  `Cost` leave the public API. Every older wire still fills them, so an agent
  cannot drop them without picking the rollout's end. Options: **when lite-06
  retires (recommended)**, since removing them now only moves them behind a
  private type; or **now**, keeping a private chain type for the older wires.
  Done when the Plan item above names the point.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract
- [Route wakes](/quest/m4/route-wakes.md) - index work follows the settled source-selection model
