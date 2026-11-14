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

Public API: none. Wire: none.

## Required

- [The router runs the path rule with a route per scope](/quest/m2/cluster-routes/scope.md) - the router `Sim` records, running the rule the model has
