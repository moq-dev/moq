# [M] Doubt holds

## Goal

A relay whose only routes left are kept off by doubt holds its claims,
unavailable, until the doubt lifts and it moves onto one, instead of letting
go while its neighbours answer that they have nothing.

## Plan

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md):
`route_failover::live_mesh_link_failure_keeps_presence` pins it on a
34-relay mesh, and the router alone does the same. Cutting every link of the
publisher's relay but its slowest leaves only routes through that relay,
which every relay that heard a link die doubts for the timer. A relay with
no usable route holds, its neighbours are in the same state and answer that
they have nothing, and it lets go before the doubt lifts: the broadcast goes
Unavailable, Offline, then Start. Over 5 publishers, 132 relays went
unavailable, 52 retracted and announced again, 98 readers lost their
subscription, and the longest gap was 2.0 s (the doubt plus the retraction
quarantine).

The contract already says a node holds "while a neighbour offers a route that
only the doubt keeps it off". Find why the answers end the hold first: a
neighbour that holds only because of doubt should answer with ROUTE_HOLD, not
nothing. Check the rule in `RouteHandoff.tla` with the `verify-route` skill
before changing `rs/moq-net/src/route/path.rs`, and build on
[moq-dev/moq#8734](https://github.com/moq-dev/moq/pull/8734), which changes
which routes doubt blocks.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - Plan item 27 and the hold rule
- [Held after close](/quest/m2/cluster-routes/held-after-close.md) - the same symptom at the relay whose own session closed
