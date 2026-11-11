# [S] A node advances its route seqno only when a route link changes

## Goal

A lite-07 node raises its own route Seqno unprompted only when a session it
exchanges routes over opens or closes, and otherwise only to answer a
ROUTE_REQUEST. A client connecting to or leaving a relay no longer makes the
relay send a ROUTE_UPDATE for its own identity on every link that carries it.
A test counts the ROUTE messages a mesh sends per client session and holds
them at zero.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing), asked when
an origin should advance its seqno unprompted: ✅ only when a route link
changes (rejected: only to answer a ROUTE_REQUEST, which makes every path
change that lengthens a route wait a round trip to the origin; keeping
today's per-stream bump until a sweep measures it). Milestone: ✅ m1
(rejected: m2 in the routes epic; m0), since relays run lite-07 in production
now and this changes no wire.

Why it exists: relays that remember a short distance to a node refuse its
new, longer path until the node issues a higher seqno. Bumping when its links
change saves the ROUTE_REQUEST round trip.

What is wrong today: `register` and `Drop for Stream` in
`rs/moq-net/src/route/node.rs` call `router.bump()` for every announce stream
of every lite-07 session, and `Routed::limit` in
`rs/moq-net/src/lite/subscriber/routed.rs` registers again on a permit
change. A relay originates the claims of its lite-06 and moq-transport
publishers under its own identity, so each client session bumps that identity
at least four times. This was read from the code, not measured: reproduce it
first with a count of ROUTE messages per client connect and disconnect on a
three-relay mesh.

Open for the implementer, with a recommendation: what counts as a route
link. Recommended: a session whose peer forwards, known because it has sent a
route to a node other than itself, plus every session of a node that does not
forward (all of a publisher's links are route links). A session that only
ever carries claims, or only the peer's own route, never bumps. Confirm the
choice against `reconnect_without_restart_needs_a_seqno_advance` and
`restart_reusing_identity_is_refused` in `route/tests/`, which the bump
exists for, and say in the PR if a narrower rule (bump on close only, since a
new link lengthens no path) also keeps them passing.

Mirror any change to `bump`'s callers in `RouteHandoff.tla` as the
`verify-route` skill says, and state the ROUTE budget per session event so
[the mesh sweep](/quest/m1/route-mesh-sweep.md) can assert it.

Public API: none. Wire: none.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the contract's seqno rule this narrows
- [Seeded lite-07 meshes hold the route rules and an announce budget](/quest/m1/route-mesh-sweep.md) - the budget that would have caught this
- [Each side says everything about routing on one stream](/quest/m2/cluster-routing/routes/route-stream.md) - registers one neighbour per session, which changes where the bump is called
