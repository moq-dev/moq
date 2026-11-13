# [XL] The router runs the path rule with a route per scope

## Goal

`route::Router` runs the rule the model
(`rs/moq-net/verify/route/RouteHandoff.tla`) settled, sans-IO, and
`route::tests::Sim` drives it. A route is keyed by its publisher and the
prefix it covers and carries its path. A relay uses the cheapest route whose
path does not name it, drops every route through a hop a withdrawal names,
holds its claims while the neighbours that learned a route from it have yet
to answer, and stays off a node in doubt until the timer lifts it. It keeps
no distance, seqno, route request, or echo.

A relay shows a claim exactly while such a route covers it and the newest
revision a covering route gave it still has the claim, two relays never keep
a dead route alive through each other, a change of path alone changes no
claim a reader sees, and a relay behind a scoped neighbour reaches what a
farther one offers. The `residual_` tests in
`rs/moq-net/src/route/tests/residual.rs` pass as plain tests on it, and a
sweep over meshes with scoped relays holds the rules at every quiet point.

Sessions, the wire, the draft, and `js/net` keep today's router until
[the switch](/quest/m2/cluster-routes/switch.md), so nothing a peer
or an application sees changes here.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan): the prototype picks the rule
for stale copies, the model settles the route per scope, and the router comes
last (rejected: the router first, patched case by case; recording the
findings as known limits and waiting for federation).

Decided 2026-11-11 (maintainer, on
[moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)): the route rule
changes too. Babel's distance, the seqno, ROUTE_REQUEST, and the ROUTE_END
echo go, for the rule in the
[epic](/quest/m2/cluster-routes/README.md)'s Plan item 27.

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel), asked how that removal is split: ✅ two quests in this epic. This one
builds the router with no wire change, and
[the switch](/quest/m2/cluster-routes/switch.md) moves the sessions,
the wire, the draft, and `js/net` onto it and deletes Babel's router
(rejected: the same split with this quest in m1, since it breaks nothing;
one quest for everything, which is one PR across the router, the wire, the
draft, the Rust sessions, and `js/net`).

Open, from the model
([moq-dev/moq#8541](https://github.com/moq-dev/moq/pull/8541)): how long a
relay keeps its doubt about a node. As the rule stands the doubt goes once no
route names the node, and a request can then loop while the news of three
closed sessions is still on its way: `Loop.cfg` breaks `RequestsLoopFree` at
depth 11 and is kept as that open finding. Keeping the doubt until its timer
fires showed no loop on that mesh in 7.6M states, to depth 13, and costs a
wait: no relay moves to a route through the doubted node until the timer
fires, a publisher reconnecting through another relay included, though a
relay keeps one it already has. Measure that reconnect delay on
the prototype's meshes under both rules and put the two to the maintainer
with the numbers. Recommended: keep the doubt, since the wait is bounded by
the timer and a looping request is not bounded by anything the relay
controls. If the rule changes,
`Loop.cfg` passes and `RequestsLoopFree` joins the rules every config checks.

The two rules share a tree until the switch. Pick how, and say why in the
PR: the prototype (`route/tests/paths.rs`, `compare.rs`, `copies.rs`)
promoted out of `tests/` into a second router, or one router generic over
the rule. Either way the claim side is one implementation, not a copy: the
views, floors, `show`, and the interest handling are what the fixtures of
the last month fixed, and the rule changes only which routes feed them.
Sessions construct today's rule and nothing on the wire changes. Whatever
of the prototype the router replaces is deleted here.

Work:

- Route selection by the rule of the epic's Plan item 27, as the model
  states it: the path with a session in each hop, the withdrawal naming a
  dead hop and listing the routes that moved, holds and their answers, and
  the doubt with its timer on the router's mocked clock. The timer's length
  starts from the maintainer's 1 s; the model and the sweeps settle it.
- Key `rx`, the selection, `sent`, and the views by route. A view then has
  one floor, so `certifiers`, `certified`, `prune`, `loose`, and the
  per-region floors have no counterpart, and the newest floor wins among one
  publisher's routes as the model has it. Keep the reader's view a function
  of what the router holds, computed incrementally the way `show` is today,
  with the test build comparing it against the whole computation.
- Keep a session's teardown about linear in the prefixes it held, as the
  routes bench's `teardown` and `refollow` cells measure
  (`rs/moq-net/benches/routes.rs`).
- What #8487 added to work around the old rule has no counterpart either:
  `recompute`'s `prune` switch, which skips pruning on a change of hops or
  RTT alone. Keep the qualified-key test in `infeasible` only if the new
  rule still needs it, and say which.
- Expose the route the model's rule picks for a request, as a crate-private
  query, so the switch can serve a request through it in `route/node.rs`'s
  `home`. `Sim` checks it reaches the publisher without a loop (`Reaches`).
- Run the handoff fixtures and both sweeps on the new rule. Flip each
  `residual_` test, and add the review harness's scoped and two-prefix
  families as a sweep beside `sweep_no_route_outgrows_a_loop_free_path`,
  checking at every quiet point that each relay shows what the rule says and
  nothing waits. Compare a few thousand scripts against today's router and
  explain every difference in the PR.
- Report route and claim messages per change on `Sim` for both rules, on the
  meshes `compare.rs` holds. The switch measures real sessions.
- Update the `verify-route` skill's coverage and sync commit for the rule
  `Sim` now runs.

Left to [the switch](/quest/m2/cluster-routes/switch.md): the wire,
the draft, `route/node.rs` and the sessions, `js/net`, the epic's contract
as built, and the deletion of today's router.

Public API: none (the router is crate-private). Wire: none.

## Related

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - puts this router on the wire and deletes the one it replaces
