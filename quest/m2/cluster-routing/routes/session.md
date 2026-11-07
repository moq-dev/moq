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
- Public API: remove `Hops`, `RouteInfo::hops`, and the saturating `Cost`
  from the public API now, keeping a private chain type for lite-06 and the
  moq-transport cluster extension, which still carry hop chains. Decided
  2026-11-07 (maintainer, /quest-unblock): ✅ now (rejected: when lite-06
  retires). It is a breaking change; the PR marks its title with `!`.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract
- [Route wakes](/quest/m4/route-wakes.md) - index work follows the settled source-selection model
