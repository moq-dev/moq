# [L] The route rules hold from any state, not only the runs TLC reaches

## Goal

An inductive invariant of the route model implies `NoRevival`, `NoFalseEnd`,
`NoRegression`, `Truthful`, and the loop rule the model proves for the path
rule: no relay uses a route whose path names it, and selected next hops loop
only within the bound the model (`rs/moq-net/verify/route/RouteHandoff.tla`)
found: through a stale route whose news is on its way (`LoopsAreStale`,
`StaleIsChased`). Apalache
checks in Nightly that it holds initially and that every step preserves it
from any state satisfying it, on each mesh shape of up to five relays. The
rules then hold for runs of any length and any number of changes, where
TLC's pass covers one claim, two revisions, and two session opens or closes.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net):

- ✅ An inductive invariant checked by Apalache (rejected: a TLAPS proof for
  every mesh size, whose hand-written steps break on each model change, until
  this invariant has settled; verifying the Rust itself in Verus or Lean,
  which needs the code rewritten around the prover).
- ✅ m1 (rejected: m4), because a flaw in the routing design is cheapest to
  fix before lite-07 is cut. It does not gate the cut: a flaw it finds becomes
  a fix quest that joins [Finalize moq-lite-07](/quest/m2/lite07-finalize.md)
  then.

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel), asked what happens to this quest now that the route rule is the path
([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27):
✅ keep it in m1, rewritten for the path rule (rejected: m4, since the path
argument is simpler than Babel's; deleting it and resting on the bounded
TLC runs and the sweeps). The claim rules are where the stale copies were,
and they are what a bounded run covers least. `LoopFree` leaves the Goal as
an invariant: under the path rule selected next hops can loop for a moment
when two unrelated things fail at once, which the maintainer accepted, so
the invariant implies the bound the model states instead.

What it removes is the bound on the run, not on the mesh: revisions,
sessions, and churn are unbounded, while the relay count and queue lengths
stay constants the config states. Say both bounds in the `verify-route` skill.

- Start from the path argument: no relay holds or selects a route whose path
  names it, every route in flight extends the path its sender held when it
  sent it, and every withdrawal in flight names a hop whose session closed.
  The doubt's timer is a step that may fire at any moment, as the model has
  it. Strengthen until Apalache finds no counterexample to induction.
- A counterexample to induction starts from a state that may be unreachable.
  If TLC cannot reach it, the invariant is too weak: strengthen it. If it can,
  replay it on `route::tests::Sim` as the skill says before calling it a bug.
- Apalache needs type annotations. Add them to the spec TLC already checks,
  so both tools read one file and neither drifts.
- `NoCountToInfinity` follows from `SimplePaths`, which is inductive once
  the invariant also says a route is held only from a node the holder shares
  a link with: every route sent adds its sender to hops that did not name it
  ([moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)). Start there.
- Apalache checks safety. That every hold ends is no checked rule: the
  model has no fairness, and the `verify-route` skill argues it from
  `Budget` and `QuietIsSettled`. Leave it there.
- Run it through the `tla` recipes with Apalache from the flake. If five
  relays does not fit Nightly, record the largest mesh that does and its time.
- A refinement check such as `RefinesClaims` runs on the smallest exhaustive
  config that exercises it, where each distinct state is checked once, not on
  a simulated config such as `Blind.cfg`, where every sampled trace re-checks
  it, unless measuring shows the simulated config is cheaper. `SumOver` folds
  with `FoldSet` through the Community Modules' Java override the `tla`
  recipes load; keep one fold for both checkers.

Public API: none. Wire: none.

## Related

- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - ties the same spec to the code
