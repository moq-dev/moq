# [M] Every delivery order of a small mesh runs on the real route layer

## Goal

A test explores every order in which `route::tests::Sim` can deliver, hold,
disconnect, collect, and change claims on a triangle of the origin and two
relays, on the real `route::Router`, and checks the route rules in every state
it reaches. A broken rule prints the path to it as `Sim` calls. The PR reports
its states and time beside TLC's for the same bounds. If the real state space
does not fit, the PR records the numbers and lands no test.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan), as a spike: `RouteHandoff.tla`
is a hand-kept mirror of the rule `route/tests/paths.rs` prototypes, which TLC interprets, while `Sim`
already drives the router one atomic step at a time. Searching `Sim` itself
runs compiled code, has no model to drift, and reaches what the model leaves
out.

- ✅ A hand-written breadth-first search beside `Sim` in `route/tests/`: a
  fingerprint set, a queue, and a parent map for the path (rejected: the
  `stateright` crate, whose 12 direct dependencies include dashmap, rand,
  serde_json, and tiny_http; building both and keeping the better). Add
  threads or symmetry only if the measurements call for them.
- Bound the run as `RouteHandoff.cfg` does (`MaxRev`, `MaxFlaps`, one
  claim), so the two counts compare.
- The search converges only if equal situations hash equal. `Sim` and `Router`
  need a clone and a fingerprint, and the fingerprint must leave out what only
  records the past, such as `events`, `stats`, and `requested`. Check each
  rule at the step that would break it rather than from that log.
- The check that the search works: run on today's router before the switch to
  the path rule, it must find the storm
  `residual_a_seqno_advance_storms_between_two_grants` replays, or another
  `residual_` test in `route/tests/residual.rs` that fits the bounds.
- Then say which of the model's "Not covered" list (`verify-route` skill) the
  search can afford: wildcards, scoped grants, costs other than 1, a fourth
  node.
- A search that takes seconds joins the merge gate; a larger one runs in
  Nightly.

This sits beside [trace validation](/quest/m1/route-trace-validation.md), which
keeps the TLA+ model honest for [the inductive invariant](/quest/m1/route-inductive.md)
and the design work done on the spec before code exists. It replaces neither
that nor the seeded sweeps on larger meshes
(`tests/integration/mesh_sweep.rs`). If the spike shows the
search covers what a model check does, recommend in the PR what to do with the
route model's TLC runs.

Public API: none. Wire: none.

## Waiting

- Maintainer: which router the search runs on. Since this quest was planned,
  the model moved to the path rule ([#8487](https://github.com/moq-dev/moq/pull/8487)),
  which `route/tests/paths.rs` prototypes, and
  [the router quest](/quest/m2/cluster-routing/routes/scope.md) rewrites
  `route::Router` to it. Options: wait for that rewrite and search the new
  `Router` (recommended: the search then checks the code that ships, and
  nothing is built twice), which moves this quest beside it in
  `quest/m2/cluster-routing/routes/` with the router quest under `Required`;
  search today's Babel `Router` now, which the rewrite discards; or search the
  `paths.rs` prototype, which only tests exercise. An agent cannot pick: it
  sets whether this spike stays in m1.

## Related

- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - guards the model against the code; this checks the code itself
- [A Rust TLA+ checker runs the models locally if it agrees with TLC](/quest/m1/tla-native-checker.md) - the other spike, a faster engine for the same specs
