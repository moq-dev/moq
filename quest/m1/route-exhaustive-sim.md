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
is a hand-kept mirror of `route/mod.rs` that TLC interprets, while `Sim`
already drives the router one atomic step at a time. Searching `Sim` itself
runs compiled code, has no model to drift, and reaches what the model leaves
out.

- ✅ A hand-written breadth-first search beside `Sim` in `route/tests/`: a
  fingerprint set, a queue, and a parent map for the path (rejected: the
  `stateright` crate, whose 12 direct dependencies include dashmap, rand,
  serde_json, and tiny_http; building both and keeping the better). Add
  threads or symmetry only if the measurements call for them.
- Bound the run as the model does: one claim, two revisions, two session opens
  or closes, one collection (`MaxRev`, `MaxFlaps`, `MaxCollects` in
  `RouteHandoff.cfg`), so the two counts compare.
- The search converges only if equal situations hash equal. `Sim` and `Router`
  need a clone and a fingerprint, and the fingerprint must leave out what only
  records the past, such as `events`, `stats`, and `requested`. Check each
  rule at the step that would break it rather than from that log.
- While [the echo fix](/quest/m2/cluster-routing/routes/echo-order.md) is
  open, the search must find a count to infinity, as
  `residual_an_overtaking_echo_counts_to_infinity` replays; once it lands, the
  search must pass. Either way that is the check that the search works.
- Then say which of the model's "Not covered" list (`verify-route` skill) the
  search can afford: wildcards, scoped grants, costs other than 1, a fourth
  node.
- A search that takes seconds joins the merge gate; a larger one runs in
  Nightly.

This sits beside [trace validation](/quest/m1/route-trace-validation.md), which
keeps the TLA+ model honest for [the inductive invariant](/quest/m1/route-inductive.md)
and the design work done on the spec before code exists. It replaces neither
that nor the random sweeps on larger meshes
([the mesh sweep](/quest/m1/route-mesh-sweep.md)). If the spike shows the
search covers what a model check does, recommend in the PR what to do with the
route model's TLC runs.

Public API: none. Wire: none.

## Related

- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - guards the model against the code; this checks the code itself
- [A Rust TLA+ checker runs the models locally if it agrees with TLC](/quest/m1/tla-native-checker.md) - the other spike, a faster engine for the same specs
