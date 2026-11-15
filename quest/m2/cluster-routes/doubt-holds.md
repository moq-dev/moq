# [M] Doubt holds

## Goal

A relay whose only routes left are kept off by doubt holds its claims,
unavailable, until the doubt lifts and it moves onto one, instead of letting
go while its neighbours answer that they have nothing.

## Plan

Kept 2026-11-15 (maintainer, /quest-complete on
[moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832)): ✅ keep it in
m2 (rejected: drop; moving it to m3).

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md):
`route::tests::flap::residual_a_relay_kept_off_by_doubt_lets_go_before_it_lifts`
pins it on the router. The publisher sits behind a transit relay that loses
most of its links, leaving only routes through it, which every relay that
heard a link die doubts for the timer. A relay with no usable route holds,
its neighbours are in the same state and answer that they have nothing, and
it lets go a hop after the cut: the claim goes Unavailable, then gone, then
back once the doubt lifts.

`route_failover::live_mesh_link_failure_keeps_presence` measured it on a
34-relay mesh before [moq-dev/moq#8734](https://github.com/moq-dev/moq/pull/8734):
over 5 publishers, 98 readers lost their subscription. That fixture publishes
on the relay itself, so since #8734 doubt no longer blocks its routes, and it
loses 6. A real-session fixture for this quest needs the publisher behind the
relay whose links close.

The contract already says a node holds "while a neighbour offers a route that
only the doubt keeps it off". Find why the answers end the hold first: a
neighbour that holds only because of doubt should answer with ROUTE_HOLD, not
nothing. Check the rule in `RouteHandoff.tla` with the `verify-route` skill
before changing `rs/moq-net/src/route/path.rs`.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - Plan item 27 and the hold rule
- [Held after close](/quest/m2/cluster-routes/held-after-close.md) - the same symptom at the relay whose own session closed
