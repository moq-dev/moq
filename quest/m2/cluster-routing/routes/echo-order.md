# [M] A ROUTE_END echo arrives behind the echoer's routes

## Goal

In the route model and `route::tests::Sim`, a ROUTE_END echo travels on the
FIFO toward the END's sender, behind every route the echoer sent first. A
node there drops its feasibility distance for an identity only once no
advertisement derived from its own can still reach it, so two relays never
count a route to infinity. Production gets the same order from
[the route stream](/quest/m2/cluster-routing/routes/route-stream.md), where
one side's echoes and routes share a stream.

## Plan

Found 2026-11-10 by the route model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/route/CountToInfinity.cfg` breaks `NoCountToInfinity` in
9 steps with no partition. A node drops its distance once every neighbour
echoed its ROUTE_END (`update` in `route/mod.rs`). But the echo rides the
announce stream the node answers, while the neighbour's own routes ride the
stream the node asked for (`route/node.rs`), so the echo can overtake an older
route the neighbour derived from the node's. The node then accepts that route
as a fresh, longer one. `residual_an_overtaking_echo_counts_to_infinity` in
`route/tests/routing.rs` replays it on the router: the metric climbs 3 to 8
over six rounds.

Decided 2026-11-10 (maintainer, /quest-plan): ✅ the neighbour sends the echo
on the stream that carries its routes to the node, queued behind every route
it sent earlier (rejected: a barrier field naming the last route sent, which
keeps the stream but adds state and a field; leaving the choice to the
implementer).

Decided 2026-11-10 (maintainer, a later /quest-plan on lite-07 routing),
asked how the echo reaches "the stream that carries the echoer's routes"
when a session holds several Announce Streams each way, or none back:
✅ **each side says everything about routing on one unidirectional stream**,
so there is exactly one such stream and the order holds by construction
(rejected: a session-scoped echo sent on every stream the echoer answers;
one bidirectional Announce Stream per direction with the echo moved to the
answering half; the barrier again). That wire change is
[the route stream](/quest/m2/cluster-routing/routes/route-stream.md)'s, and
it is where the draft, the session code, and js/net change. This quest is
the model and `Sim` half, which
[moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487) holds, and it
lands first so [the interest set](/quest/m2/cluster-routing/routes/interest-set.md)
can build on it.

Done when `residual_an_overtaking_echo_counts_to_infinity` becomes a test
that the metric never passes the longest loop-free path, `NoCountToInfinity`
moves into `RouteHandoff.cfg`, `CountToInfinity.cfg` is deleted, and the
`verify-route` skill's open finding and sync commit are updated. The epic's
loop-freedom bullet then states the echo ordering it relies on, and that
production does not have it until the route stream lands.

Caused by: [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), which
put the echo on the answering stream; the drain rule itself came from
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676).

Public API: none. Wire: none.

## Related

- [The route model covers a mesh with clients and an announce budget](/quest/m1/route-mesh-model.md) - checks the same drain under unequal link costs, which the triangle with unit costs cannot reach
