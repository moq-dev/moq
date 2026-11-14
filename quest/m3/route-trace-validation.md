# [L] Route sweeps are checked against the TLA+ model

## Goal

Every run of the route layer's random sweeps (`sweep_handoff`,
`sweep_scoped_handoff`) is also a behaviour of `RouteHandoff.tla`: a code
change the model does not allow fails the sweep, without anyone remembering to
resync the model.

## Plan

Moved from m1 to m3 2026-11-13 (maintainer, /quest-plan): it waits on m2
work, and no quest requires one in a later milestone, since a blocker ranks
by its own milestone. ✅ m3 (rejected: m2, where the cut would wait on it;
pulling the m2 blocker into m1). It ranks right after m2, when it can start.

Decided 2026-11-10 (maintainer, /quest-plan), as the strongest guard against
the route model drifting from the code ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)).

Decided 2026-11-10 (maintainer, a later /quest-plan the same day):

- ✅ m1 (rejected: staying in m4; superseded 2026-11-13: m3, above). The mesh configs beside it, the
  inductive invariant (dropped 2026-11-13: no checker can step the model), and the
  [cache-aware routing proof](/quest/m2/cache-routing-proof.md) all build on
  `RouteHandoff.tla`, so drift there costs the most.
- ✅ The other three models get trace validation too, each its own m4 quest
  behind this one (rejected: "route only for now; judge the other models
  after", decided earlier the same day). So the recorder's trace format and
  the recipe that hands a trace to TLC are built here for
  [resume](/quest/m4/resume-trace-validation.md),
  [reconnect](/quest/m4/reconnect-trace-validation.md), and
  [subscribe](/quest/m4/subscribe-trace-validation.md) to reuse.

This is TLA+ trace validation (Cirstea, Kuppe, Loillier, Merz, 2024, used on
CCF and etcd's raft): `route::tests::Sim` records each step as the model
action it corresponds to, with the variables the model keeps, and TLC checks
that the recorded trace is a behaviour of the spec. Where the model covers
less than the sweeps do, the sweeps either restrict to what it models or the
model grows to cover them; pick whichever costs less and say which.

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel): the traces come from the path router
([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27).
Once the model has the path rule, a trace of Babel's router is no behaviour
of it, so this waits for the router too.

Run it on the sweeps' existing seeds in CI only if it stays fast; otherwise in
Nightly with the sweeps' fresh seeds.

Paused 2026-11-14 when the quest moved to m3, with the work done on its
branch ([moq-dev/moq#8733](https://github.com/moq-dev/moq/pull/8733)), taken
under /quest-afk for the maintainer's review:

- Done: `route::tests::Sim` records each path-rule run of both sweeps
  (`rs/moq-net/src/route/tests/trace.rs`), `rs/moq-net/verify/route/trace/RouteTrace.tla`
  follows it step by step comparing every message and each touched relay's
  route and shown claims, `sh/tla-trace.py` runs TLC per trace, and
  `test route-trace` (Nightly, a fresh window of 150 scenarios) wires it up.
  All 300 traces of the gate's scenarios pass on the pinned TLC, and two of
  four planted router bugs fail traces while the sweeps pass.
- ✅ Restrict the sweeps, and grow the model only where the router had
  drifted (rejected: growing the model to several publishers, restarts,
  grants, and interest edits, which four open PRs on `RouteHandoff.tla`
  would conflict with). `sweep_handoff` runs one origin whose every claim change
  changes a claim, with no restart; `the_rules_agree_on_every_script` keeps the
  full script. The client, a leaf that offers no route, is dropped from the
  traces. The model grew three ways: a route goes to a stream only while a
  claim meets it, timers due together fire in one `Timeout`, and an ended
  stream forgets what it reflected. Every config still passes.
- ✅ Nightly only (rejected: the merge gate): 300 traces cost about 20
  core-minutes of TLC.
- Found: `Stable` breaks on an accepted trace, a behaviour of the model the
  configs miss ([stable-repath](/quest/m2/cluster-routes/stable-repath.md)),
  so `RouteTrace.cfg` leaves it out.

Left: run `just fix` and `just check`, refresh the skill's state-count table
for the grown model (`RouteHandoff.cfg` now 29,229 distinct states; the
configs' new counts are in each run's output), run `test route-trace` through
`sh/tlc.py` once Nix fetches TLC again (this host's daemon hung on it, so the
runs used the same jars by hand), review with `/quest-review`, and finish the
PR body.

Public API: none. Wire: none.
