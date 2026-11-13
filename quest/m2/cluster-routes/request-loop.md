# [M] A request loop heals once routes settle

## Goal

A subscription that loops between relays while routes settle reaches the
publisher once the next hops stop looping, instead of hanging silently.
Under the path rule, selected next hops can loop for a moment
([epic](/quest/m2/cluster-routes/README.md) Plan item 27). A request
forwarded around that loop meets a relay already requesting the same track
upstream, and aggregation joins it to that request. Nothing errors, so a
pending request that never follows its route's change waits forever with
no data and no failure.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on the BGP failure modes):

- ✅ A fixture on real relays, with no TLA+ (rejected: adding pending
  requests and aggregation to `RouteHandoff.tla`). The model's
  `RequestsLoopFree` already checks next hops; this checks that the
  relay's request state follows them.
- ✅ If the fixture hangs, a relay's pending upstream request follows its
  route when the selected next hop changes, even before the request is
  answered, so the loop clears with the routes (rejected: a hop count on
  every request that fails one at the limit, which adds a field to SUBSCRIBE,
  FETCH, and the other requests and turns a loop that would heal into an
  error the subscriber sees). Epic Plan item 27 already rejected keeping a
  request from going back through the relay that sent it.

The fixture runs real lite-07 relays with mocked time. Form the loop the
churn sweeps found (two unrelated failures at once, so two relays briefly
route a publisher through each other), subscribe at both while it lasts,
then let the routes settle. Both subscribers receive the next group, and no
upstream request is left pending. Cover a request answered before the loop
and one still unanswered when it forms. Check which relay state carries the
pending request (the front in `rs/moq-net/src/model/front.rs`, and how
`route/node.rs` `home`s it) before choosing where the fix goes.

## Required

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - the sessions whose next hops can loop

## Related

- [A relay that refuses a forwarded route drops the route, not the session](/quest/m1/refuse-route.md) - another failure mode from the same BGP comparison
- [A dual-homed publisher stays reachable while one link flaps](/quest/m2/cluster-routes/flapping-publisher.md) - another failure mode from the same BGP comparison
