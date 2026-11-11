# [L] Route sweeps are checked against the TLA+ model

## Goal

Every run of the route layer's random sweeps (`sweep_handoff`,
`sweep_scoped_handoff`) is also a behaviour of `RouteHandoff.tla`: a code
change the model does not allow fails the sweep, without anyone remembering to
resync the model.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan), as the strongest guard against
the route model drifting from the code ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)).
Route only for now; judge the other models after.

This is TLA+ trace validation (Cirstea, Kuppe, Loillier, Merz, 2024, used on
CCF and etcd's raft): `route::tests::Sim` records each step as the model
action it corresponds to, with the variables the model keeps, and TLC checks
that the recorded trace is a behaviour of the spec. The model fixes interest
at the root and drops scopes, so the sweeps either restrict to what it models
or the model grows to cover them; pick whichever costs less and say which.

Run it on the sweeps' existing seeds in CI only if it stays fast; otherwise in
Nightly with the sweeps' fresh seeds.

Related: [the mesh route model](/quest/m1/route-mesh-model.md), which grows
the spec these traces are checked against.

Public API: none. Wire: none.
