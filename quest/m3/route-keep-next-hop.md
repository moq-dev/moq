# [S] A relay keeps its next hop among equal routes

## Goal

A lite-07 relay that already routes to an origin through one neighbour stays
on it when another neighbour offers a route of the same rank: the same drain
state, priority, and metric. Today the lower key wins, so a relay moves for
nothing, and since routes name the nodes they crossed
([moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)) every relay
downstream is sent a ROUTE_UPDATE for it. A test brings a link up beside an
equal one and counts the route messages sent on every other session: none.
The new link's own first advertisements do not count.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan after #8487): ✅ plan it, in m3
beside [split horizon](/quest/m3/route-split-horizon.md), since it trims
traffic and fixes nothing (rejected: dropping it).

What it costs today, from #8487's measurements: a change of hops alone is
3.5% to 11% of route messages in the router's churn sweeps, about 3% of
control messages over real sessions, and on a 32-relay full mesh one link
coming up costs 592 route messages against 251 before the hops.

[Cluster routing](/quest/m3/cluster-routing/README.md) already decided that
"a new connection alone does not outrank a different equal-cost route", so
this implements a settled rule. Keep the deterministic path hash among equal
routes for a request's first hop: that choice is per request and is not this
selection.

- `selected` in `route/mod.rs` is a function of what the router holds, and
  ranks by key last. Staying put makes it depend on the previous choice, so
  keep the current next hop per identity and prefer it among equals. Mirror
  it in `RouteHandoff.tla`, whose `Best` picks the lowest key, as the
  `verify-route` skill says.
- Two routes of the same rank can still differ in whether they are
  down-only, which decides whether an upstream neighbour is sent the route
  at all. Stay put only among routes that agree on it, so a relay on a
  down-only route still moves to an equal one it may export, and test that.
- Check that every router test that names the next hop it expects still
  means what it says, rather than editing expectations to match.
- How a route is selected is being replaced
  ([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27).
  Apply this to the rule that stands when this starts. The prototype in
  `route/tests/paths.rs` already stays on its next hop among equals.
- Measure with `Sim`'s `hop_updates` count and, if it has landed,
  `mesh_sweep.rs`'s benchmark, before and after.

Public API: none. Wire: none; behaviour only.

## Related

- [Relays split horizon per session](/quest/m3/route-split-horizon.md) - the other trim of route traffic
