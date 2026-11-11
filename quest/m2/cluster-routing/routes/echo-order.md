# [M] A ROUTE_END echo arrives behind the echoer's routes

## Goal

A lite-07 node drops its feasibility distance for an identity only once no
advertisement derived from its own can still reach it. Two relays never count
a route to infinity, and a relay never shows a broadcast available through a
route that loops back to itself.

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
implementer). This is a lite-07 wire change; lite-07 is still `-wip`, so it
changes freely, and the draft changes in the same PR. Mirror it in js/net's
port ([js/net route layer](/quest/m2/cluster-routing/routes/js.md)); whichever
lands second adapts.

Done when `residual_an_overtaking_echo_counts_to_infinity` becomes a test
that the metric never passes the longest loop-free path, `NoCountToInfinity`
moves into `RouteHandoff.cfg`, `CountToInfinity.cfg` is deleted, and the
`verify-route` skill's open finding and sync commit are updated. The epic's
loop-freedom bullet then states the echo ordering it relies on.

Caused by: [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), which
put the echo on the answering stream; the drain rule itself came from
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676).

Public API: none. Wire: lite-07-wip only.
