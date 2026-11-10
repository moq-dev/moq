# [M] The route node behaves the same whoever creates it

## Goal

An origin's lite-07 route node, and the own-claims feeder that tells it which
routes this node serves itself, behave the same no matter which handle or
which code path creates them first. Creating the node at session start (for
example from SETUP) instead of from the route layer leaves every existing
test passing, including the three `socket::` dial-by-hand tests, where
`room/.server/bob` must stay routable. Neither the public API nor the wire
changes.

## Plan

Found while building [moq-dev/moq#8325](https://github.com/moq-dev/moq/pull/8325),
which works around it: its SETUP reads `origin::Consumer::node_id()` (new
there, `rs/moq-net/src/lite/session.rs:270-276` on its branch) so it never
creates the node. Its notes report that calling `origin.route_node()` at
lite-07 session start instead deterministically broke
`a_second_tcp_group_resets_the_session`,
`a_second_tcp_group_after_fin_resets_the_session`, and
`a_peer_reset_after_fin_resets_the_session`
(`rs/moq-net/tests/integration/socket.rs:344,371,392`), whose
`dial_by_hand` request for `room/.server/bob` (`socket.rs:307`) then
fails, whichever handle created it. Reproduce that first; the cause is not
yet known.

What is verified (line numbers on `main` at `10772522a5`):

- Nothing creates the node at session start today, and no cluster code sets
  it. It is created lazily by the first `Consumer::route_node()` call
  (`rs/moq-net/src/model/origin.rs:7177`), which the route layer makes from
  `lite/publisher/routed.rs:165`, `lite/subscriber/routed.rs:120` and `:799`,
  `lite/publisher.rs:210` (a request naming a node), and
  `lite/publisher.rs:3792`. So which session, and which of these paths,
  creates it depends on timing.
- `create_route_node` (`origin.rs:7189-7230`) builds the feeder's two cursors
  from the creating handle with `..self.clone()`. It resets scope, root,
  horizon, covering, stats, hidden, and interest, but keeps that handle's
  `proxy` (set per handle by `Producer::with_proxy`, `origin.rs:2235`), pool,
  cache duration, max hops, task set, and timers. Anything per-handle that
  leaks into the feeder makes its output depend on the creator.

Candidate causes to check against the reproduction: a per-handle field the
feeder inherits; the feeder's cursors opened before the first routes land in
the table, so `catch_up` misses something the driver never feeds; or the
node's identity being fixed before the session's own hop or exclusion is
known. Fix the cause in `create_route_node` so the feeder is built from
origin-wide state only (e.g. from the shared state or the producer's config,
not the calling handle), rather than by pinning creation to one call site.

Add a test that creates the node early, from a handle with non-default
per-handle settings, and asserts the same routing as lazy creation. Once
fixed, `node_id()` may simplify to reading `route_node().declared()` if #8325
has landed; keep it only if creating the node there still costs something.

Likely caused by [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023),
which added `create_route_node` with `..self.clone()` and lazy creation from
the route layer; confirm once the reproduction names the cause.

Decisions:

- ✅ Milestone m2, under `quest/m2/cluster-routing/routes/` (rejected: m1;
  dropping it). Maintainer, 2026-11-09, `/quest-complete` follow-up of
  [moq-dev/moq#8325](https://github.com/moq-dev/moq/pull/8325).

Impact: no public API or wire change; `route_node` and `create_route_node`
are crate-private.

## Related

- [Route layer acceptance](/quest/m2/cluster-routing/routes/acceptance.md) - proves the route layer on the implementation, which this ordering bug would undermine
- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the epic whose route layer owns the node
