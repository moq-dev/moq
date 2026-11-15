# [L] Route fan-out

## Goal

A route change costs a relay work in the streams and broadcasts it touches:
not every answering stream, not every claim behind the route, and not state
remembered for sessions nothing was learned on.

## Plan

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md)'s
operation-count regressions, each pinned by a `residual_*` test that fails
once fixed:

- Unrelated streams (`route::tests::cost::residual_a_route_change_visits_every_unrelated_stream`,
  `residual_closing_a_stream_revisits_every_other_stream`): every route change
  visits every answering stream for each identity it recomputes, and changes
  with no single route in scope (RTT, DEAD, drain, close) plan every prefix
  of each stream. Closing one viewer costs the origins it watched times all
  other streams, so viewer churn is quadratic. Measured: an RTT change with
  1,000 origins and 1,000 unrelated streams takes 847 ms. Recommended: index
  the answering streams in `rs/moq-net/src/route/path.rs` by the prefixes
  they ask for and by the routes sent to them.
- Per-broadcast rewrites (`route::node::tests::residual_a_route_update_reserves_every_claim`):
  in `rs/moq-net/src/route/node.rs`, any change to a route, its RTT included,
  re-serves every claim behind it into the origin (`Node::home`), a
  route-table write per broadcast that wakes the fronts below it. The wire
  stays flat. Recommended: one shared record per route in the origin that its
  broadcasts refer to, coordinated with [route wakes](/quest/m4/route-wakes.md).
- History-bound state (`residual_same_clock_churn_is_remembered_until_its_timer`,
  `route::node::tests::residual_each_closed_session_is_remembered_until_the_doubt_timer`):
  dead hops, doubts, and held routes grow with the churn inside one timer,
  and a closed viewer session that sent no route still leaves a dead hop (and
  a doubt if its client declared a node). Skipping the dead hop for a session
  nothing was learned on looks safe, since no path can name it; check it with
  the `verify-route` skill first.

Build on [moq-dev/moq#8734](https://github.com/moq-dev/moq/pull/8734),
which also edits `path.rs`.

## Related

- [Route wakes](/quest/m4/route-wakes.md) - origin-side wakes per route change
- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract's cost bullets
