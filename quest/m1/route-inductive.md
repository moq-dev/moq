# [L] The route rules hold from any state, not only the runs TLC reaches

## Goal

An inductive invariant of the route model implies `LoopFree`, `NoRevival`,
`NoFalseEnd`, `NoRegression`, and `Truthful`. Apalache checks in Nightly that
it holds initially and that every step preserves it from any state satisfying
it, on each mesh shape of up to five relays. The rules then hold for runs of
any length and any number of changes, where TLC's pass covers one claim, two
revisions, and two session opens or closes.

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

What it removes is the bound on the run, not on the mesh: sequence numbers,
revisions, and churn are unbounded, while the relay count and queue lengths
stay constants the config states. Say both bounds in the `verify-route` skill.

- Start from the argument Babel and DUAL rest on: along selected next hops the
  (seqno, distance) pair strictly improves, and every message in flight carries
  a value no better than its sender's feasibility distance when it was sent.
  Strengthen until Apalache finds no counterexample to induction.
- A counterexample to induction starts from a state that may be unreachable.
  If TLC cannot reach it, the invariant is too weak: strengthen it. If it can,
  replay it on `route::tests::Sim` as the skill says before calling it a bug.
- Apalache needs type annotations. Add them to the spec TLC already checks,
  so both tools read one file and neither drifts.
- `NoCountToInfinity` follows from `SimplePaths`, which is inductive on its
  own: every route sent adds its sender to hops that did not name it
  ([moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)). Start there.
- Run it through the `tla` recipes with Apalache from the flake. If five
  relays does not fit Nightly, record the largest mesh that does and its time.

Public API: none. Wire: none.

## Required

- [The route model covers a mesh with clients and an announce budget](/quest/m1/route-mesh-model.md) - the spec parametrised over mesh shapes that this invariant is stated on

## Related

- [The TLA+ models run nightly and reviews resync them](/quest/m1/model-checks-nightly.md) - the Nightly job this check joins
- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - ties the same spec to the code
