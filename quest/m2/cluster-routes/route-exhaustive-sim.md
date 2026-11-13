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
is a hand-kept mirror of `route::path::Router` in `rs/moq-net/src/route/path.rs`, which TLC interprets, while `Sim`
already drives the router one atomic step at a time. Searching `Sim` itself
runs compiled code, has no model to drift, and reaches what the model leaves
out.

- ✅ A hand-written breadth-first search beside `Sim` in `route/tests/`: a
  fingerprint set, a queue, and a parent map for the path (rejected: the
  `stateright` crate, whose 12 direct dependencies include dashmap, rand,
  serde_json, and tiny_http; building both and keeping the better). Add
  threads or symmetry only if the measurements call for them.
- Bound the run as `RouteHandoff.cfg` does (`MaxRev`, `MaxFlaps`, one
  claim), but on the triangle: `RouteHandoff.cfg` has four nodes, so the PR
  runs TLC on a copy of that config cut to the same three nodes, and the two
  counts compare.
- The search converges only if equal situations hash equal. `Sim` and `Router`
  need a clone and a fingerprint, and the fingerprint must leave out what only
  records the past, such as `events`, `stats`, and `requested`. Check each
  rule at the step that would break it rather than from that log.
- The check that the search works: run on today's router before the switch to
  the path rule, it must find the storm
  `distance::a_seqno_advance_storms_between_two_grants` replays, or another
  of today's bugs in `route/tests/residual.rs`. Those fixtures use 6 to 10
  nodes, beyond the triangle, so the self-check searches the fixture's own
  mesh, bounded by depth and by the fixture's scripted claims, rather than
  the triangle's bounds; the search code takes the mesh as a parameter.
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

Decided 2026-11-12 (maintainer, pick round): the search runs on the `Router`
after [the router quest](https://github.com/moq-dev/moq/pull/8622)'s rewrite to
the path rule (`route::path::Router`), so it checks the code that ships and nothing is built twice
(rejected: today's Babel `Router`, which the rewrite throws away; the test-only
`paths.rs` prototype). The storm self-check above then runs against a router
from before the rewrite.

Public API: none. Wire: none.

## Related

- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - guards the model against the code; this checks the code itself
- [A Rust TLA+ checker runs the models locally if it agrees with TLC](/quest/m1/tla-native-checker.md) - the other spike, a faster engine for the same specs
