# [XL] The router certifies claims by path, not by distance

## Goal

`route::Router` believes a neighbour's copy of a publisher's claims when that
neighbour's hops do not name the router, as
[the model](/quest/m2/cluster-routing/routes/certify-model.md) settled. A
relay shows a claim exactly while such a neighbour certifies it, what it shows
depends only on what its neighbours currently send, and a seqno advance alone
changes nothing a reader sees. The `residual_` tests that quest pinned pass as
plain tests, a sweep over meshes with scoped relays holds the rules at every
quiet point, and the draft and the epic's contract say what the router does.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan after
[moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)'s reviews): the
work is two quests, the rule in the model first and the router second
(rejected: the model alone, deciding the router from what it shows; recording
the findings as known limits and waiting for federation). The decisions and
the rule are in the model quest's Plan and the epic's Plan item 26.

Work:

- Replace `certifiers`, `certified`, `prune`, `loose`, and the merged `views`
  in `route/mod.rs` with the settled rule. Keep the reader's view a function
  of what the router holds, computed incrementally the way `show` is today,
  with the test build comparing it against the whole computation.
- Remove what #8487 added to work around the old rule: `recompute`'s `prune`
  switch, which skips pruning on a change of hops or RTT alone. Keep the
  qualified-key test in `infeasible` unless the new rule makes it moot, and
  say which.
- Flip each `residual_` test from the model quest, and add the review
  harness's scoped and two-prefix families as a sweep beside
  `sweep_no_route_outgrows_a_loop_free_path`, checking at every quiet point
  that each relay shows what the rule says and nothing waits. Compare a few
  thousand scripts against the router before this change and explain every
  difference in the PR.
- Draft: Routing and Floors say who certifies, in place of "a peer holds a
  route whose last advertisement is feasible here" wherever that sentence
  means certification. Add a changelog bullet. If the rule changes what a
  ROUTE or ANNOUNCE_FLOOR on a stream means, that is a lite-07-wip wire
  change: list it under Impact and run `just test interop --all`.
- If [js/net's route layer](/quest/m2/cluster-routing/routes/js.md) has
  landed, change it in the same PR.
- Rewrite the epic's certification bullet as built, and update the
  `verify-route` skill's coverage and sync commit.
- Measure route and claim messages per change with
  `rs/moq-net/tests/integration/mesh_sweep.rs`'s benchmark before and after,
  if it has landed.

Public API: none (the router is crate-private). Wire: lite-07-wip, only if the
model quest's rule needs it.

## Required

- [The route model certifies claims by path](/quest/m2/cluster-routing/routes/certify-model.md) - the rule, proven on two prefixes and a scoped relay, and the tests that pin today's bugs
