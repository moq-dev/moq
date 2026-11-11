# [L] The route model covers a mesh with clients and an announce budget

## Goal

The route model checks lite-07 routing on meshes larger than a triangle, with
more than one origin and with clients, and adds the two rules production has
broken most: a change costs a bounded number of announce messages, and
routing goes quiet after the last change.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan), after the triangle model in
[moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447) found count to
infinity. Routing, announce spam, and resume failover have caused many
production issues; this and [the lite-07 mesh sweep](/quest/m1/route-mesh-sweep.md)
are routing's confidence gates for finalizing lite-07.

- Grow `RouteHandoff.tla` (or a sibling spec sharing its operators) to 4 and
  5 relays in a line, a ring, and a ring with a chord; two origin identities;
  and client leaves that advertise nothing but echo every ROUTE_END.
- An invariant bounds the ROUTE, batch, and request messages sent between
  external changes (an origin's claim change, a session open or close) by a
  budget derived from the links, so announce spam is a counterexample.
- A liveness property, under weak fairness of delivery: after the last
  external change, the queues empty and every relay shows what
  `QuietIsSettled` requires.
- Keep explicit-state runs tractable with `SYMMETRY` over interchangeable
  relays; run larger meshes with TLC's random simulation in Nightly
  ([nightly models](/quest/m1/model-checks-nightly.md)).
- Count to infinity stays in its own open-finding config until
  [the echo fix](/quest/m2/cluster-routing/routes/echo-order.md) lands, so
  the other rules still run on larger meshes.
- Decided 2026-11-10 (maintainer, a later /quest-plan on lite-07 routing):
  the model takes a cost per link, since deployments price links, and always
  includes an origin that is not adjacent to every relay. The triangle with
  the origin as one of its nodes cannot reach the count to infinity found
  that day, which needs only an origin behind a relay; the echo fix adds the
  costs and that shape, and this keeps both on every larger mesh.

Replay every counterexample on `route::tests::Sim` before calling it a bug, as
the `verify-route` skill says, and pin confirmed ones as `residual_*` tests.

Public API: none. Wire: none.
