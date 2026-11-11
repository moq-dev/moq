# [L] A node forgets a route only once nothing derived from it remains

## Goal

A lite-07 node drops its feasibility distance for an identity only once no
advertisement derived from its own exists anywhere, on a link or held by
another relay. The router, `route::tests::Sim`, and the route model agree on
the rule, and TLC finds no count to infinity on meshes where the origin sits
behind a relay, with unit and with unequal link costs. A dead publisher goes
offline everywhere and routing goes quiet.

## Plan

Found 2026-11-10 by the route model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/route/CountToInfinity.cfg` breaks `NoCountToInfinity` in
9 steps with no partition. A node drops its distance once every neighbour
echoed its ROUTE_END (`update` in `route/mod.rs`). But the echo rides the
announce stream the node answers, while the neighbour's own routes ride the
stream the node asked for (`route/node.rs`), so the echo can overtake an older
route the neighbour derived from the node's.

Decided 2026-11-10 (maintainer, /quest-plan): ✅ the neighbour sends the echo
on the stream that carries its routes to the node, queued behind every route
it sent earlier (rejected: a barrier field naming the last route sent;
leaving the choice to the implementer).
[moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487) did that in the
model and `Sim`. In production the order comes from one unidirectional
Announce Stream per side
([the route stream](/quest/m2/cluster-routing/routes/route-stream.md),
decided the same day), not from moving the echo between today's streams.

Found 2026-11-10, a later /quest-plan on lite-07 routing: **that order is
not enough.** With the echo on the neighbour's FIFO, the model still counts
to infinity as soon as the origin is not adjacent to every relay. An echo
proves only that nothing derived is in flight on that one link; a derived
copy can sit at a third relay and arrive over two hops. The shortest case,
an origin behind relay A with A, B, and C in a triangle and unequal costs:

1. The origin announces. A advertises to B and C, and B and C to each other.
2. The origin's session closes. A sends ROUTE_END to B and C.
3. B and C each apply it and echo. Their earlier ROUTEs to each other are
   still queued.
4. A has both echoes and drops its distance.
5. B receives C's old ROUTE, which is feasible there, selects it, and
   advertises it to A, which accepts it.
6. The route laps A, C, B, A, each relay draining and accepting again, and
   the metric climbs without bound.

With unit costs the same count needs only a delayed ROUTE and three relays
that drain in turn. #8487's triangle passes because its origin is one of the
three nodes and drops what it hears for its own identity. The branch
`route-drain-counterexamples` (commit `c6ccf3f71c`, built on #8487's head)
holds per-link costs in the model, the two failing configs (`DrainCosts`, 20
states, and `DrainUnit`, 23 states), and four `Sim` replays that follow the
traces step for step. No forwarding loop closed in them; the harm is a dead
origin shown and routed to forever.

Decided 2026-11-10 (maintainer, the same /quest-plan), asked how a relay
safely forgets a publisher that is gone: ✅ **a relay acknowledges a
ROUTE_END only when its own downstream has.** A node that was routing
through the sender echoes its ROUTE_END only once every ROUTE_END it sent in
turn has been echoed; a ROUTE_END from any other neighbour is echoed at
once, so a cycle cannot deadlock. A node then drains only when nothing
derived from its route remains anywhere. Rejected: never dropping the
distance, about 16 bytes per publisher ever seen, which grows without bound
since every publisher is an origin node (epic item 18) and which a peer can
grow on purpose; a timer as in RFC 8966, whose safety rests on stale
messages being gone by then; and making only relays origin nodes so that
never forgetting costs nothing, rejected again.

That rule is not modelled yet, and it is the most intricate of the options,
so the model goes first:

- Put the rule in `RouteHandoff.tla` with per-link costs, add the origin
  behind a triangle (both cost sets) to the checked configs, and run until
  `NoCountToInfinity`, `LoopFree`, and `QuietIsSettled` pass at full bounds.
  State exactly which neighbours a node waits on when its fallback is
  feasible and it sends no ROUTE_END of its own; that case is where the
  count above starts.
- Check a ring of four with no origin leaf and a second origin placement, as
  far as explicit-state runs allow, and say what stays unexplored.
- Prove a peer that withholds echoes still holds no more than today: one end
  per node per stream.
- Then the router and `Sim`, with the four replays turned into tests that the
  metric never passes the longest loop-free path. If the rule cannot be made
  to pass, stop and report it rather than weakening a rule.
- Update the draft's Routing text for when an echo is sent; the message and
  its fields do not change. Update the `verify-route` skill's open finding,
  coverage, and sync commit, and the epic's loop-freedom bullet and its
  decision 6, which says a per-neighbour echo is enough.

Caused by: [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), which
put the echo on the answering stream, and
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676), whose drain
rule assumed a per-neighbour echo proves no derived route remains.

Public API: none. Wire: lite-07-wip, the timing of the echo only.

## Related

- [The route model covers a mesh with clients and an announce budget](/quest/m1/route-mesh-model.md) - grows the same spec to larger meshes; it keeps the per-link costs this adds
