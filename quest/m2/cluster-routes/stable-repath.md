# [M] A relay keeps its claims when its next hop re-paths through a dead hop

## Goal

A relay whose next hop sends a new path through a session the relay already
knows has closed keeps showing its claims, unavailable, until that next hop
answers, instead of dropping them offline on that one message. The route
model's `Stable` holds on every run of the router's sweeps, and
`RouteTrace.cfg` checks it again.

## Plan

Found by trace validation ([moq-dev/moq#8733](https://github.com/moq-dev/moq/pull/8733)):
in `sweep_handoff` scenario 139 (`MOQ_ROUTE_TRACE=<dir> just rs test -p moq-net --lib -E 'test(=route::tests::handoff::path::sweep_handoff)'`,
then `python3 sh/tla-trace.py rs/moq-net/verify/route/trace/RouteTrace.tla <dir>/handoff-139.ndjson`
with `Stable` back in `RouteTrace.cfg`), the router and the model take the same
steps, and at step 87 relay 4 drops two available claims
offline. Relay 4 routes through relay 0 and has already heard the DEAD for
relay 2's closed session to the publisher. Relay 0 has not, and re-paths
through relay 2's closed session. The new offer is dead at relay 4, so it has no
usable route. It holds nothing, because its next hop's path is no longer the
one its route was built on, and it has no child or doubted offer to wait for.
The claims come back once relay 0 hears the DEAD and moves. That is the flap
`Stable` rules out. It needs six nodes and a message order no config reaches.

Recommended: a relay holds under its next hop while that hop's offer is dead,
as it does while the hop holds the path the route was built on, since the DEAD
the relay heard is on its way to that hop (`StaleIsChased`). Check that holds
still never close a cycle (`HoldCycle.cfg`, `QuietIsSettled`), add a config
that reaches the flap, and change the router and model together.

Public API: none. Wire: none.

## Waiting

- Maintainer: hold under a next hop whose new path is dead (recommended, as
  above), or accept the flap and narrow `Stable` to exempt a path change that
  makes the route dead, which keeps the rule as it is but lets a stale offer
  flap what a reader sees until the DEAD catches up. Decide in the pick round
  for moq-dev/moq#8733.
