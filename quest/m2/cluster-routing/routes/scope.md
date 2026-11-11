# [XL] The router keeps a route per scope

## Goal

`route::Router` keys a route by its publisher and the prefix it covers, and
believes a route that did not cross it, as
[the model](/quest/m2/cluster-routing/routes/scope-model.md) settled. A relay
shows a claim exactly while such a route covers it and the newest revision
the relay knows still has the claim, two relays never keep a
dead route alive through each other, a change of path or seqno alone changes
no claim a reader sees, and a relay behind a scoped neighbour reaches what a
farther one offers. The `residual_` tests
[the prototype](/quest/m2/cluster-routing/routes/stale-copies.md) pinned pass
as plain tests, a sweep over meshes with scoped relays holds the rules at
every quiet point, and the draft and the epic's contract say what the router
does.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan): the prototype picks the rule
for stale copies, the model settles the route per scope, and the router comes
last (rejected: the router first, patched case by case; recording the
findings as known limits and waiting for federation).

Work:

- Key `rx`, the selection, `sent`, and the views in `route/mod.rs` by route.
  A view then has one floor, so delete `certifiers`, `certified`, `prune`,
  `loose`, and the per-region floors, and keep the newest floor among one
  publisher's routes as the model has it. Keep the reader's view a function
  of what the router holds, computed incrementally the way `show` is today,
  with the test build comparing it against the whole computation.
- Remove what #8487 added to work around the old rule: `recompute`'s `prune`
  switch, which skips pruning on a change of hops or RTT alone. Keep the
  qualified-key test in `infeasible` unless the new rule makes it moot, and
  say which.
- Serve a request through the route the model's rule picks, in
  `route/node.rs`'s `home`, in place of every feasible route whose neighbour
  `carries` the claim.
- Flip each `residual_` test, and add the review harness's scoped and
  two-prefix families as a sweep beside
  `sweep_no_route_outgrows_a_loop_free_path`, checking at every quiet point
  that each relay shows what the rule says and nothing waits. Compare a few
  thousand scripts against the router before this change and explain every
  difference in the PR.
- Wire (lite-07-wip): whatever the model settled for naming a route's
  prefix. List it under Impact and run `just test interop --all`.
- Draft: Routing says a route covers a prefix, and Floors says a floor
  certifies its route's prefix. Replace the Floors condition "every peer
  whose last advertised route to it is feasible here has answered the
  current request" with the settled rule. The hold rule in Routing is not
  about belief and stays unless the prototype's pick removes holds. Add a
  changelog bullet.
- If [js/net's route layer](/quest/m2/cluster-routing/routes/js.md) has
  landed, change it in the same PR.
- Rewrite the epic's bullet on who certifies as built, and update the
  `verify-route` skill's coverage and sync commit.
- Measure route and claim messages per change with
  `rs/moq-net/tests/integration/mesh_sweep.rs`'s benchmark before and after.

Public API: none (the router is crate-private). Wire: lite-07-wip.

## Required

- [The route model has a route per scope](/quest/m2/cluster-routing/routes/scope-model.md) - the rule, proven on two prefixes and a scoped relay
