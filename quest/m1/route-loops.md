# [L] The route model's loop rules hold for runs of any length

## Goal

`SimplePaths`, `LoopsAreStale`, and `StaleIsChased` from
`rs/moq-net/verify/route/RouteHandoff.tla` hold for runs of any length:
Apalache checks in Nightly that an inductive invariant of a model with
routes, DEAD, holds, and doubts, but no claims, holds initially and that
every step keeps it. TLC checks on the route model's configs that
`RouteHandoff.tla` refines it, as it does for the claims model
(`RouteClaims.tla`, `RefinesClaims`). The bounds are the relay count and the
messages in flight the configs state; revisions, sessions, and churn are
unbounded. The `verify-route` skill states both bounds.

## Plan

Decided 2026-11-12 (maintainer, /quest-unblock on
[moq-dev/moq#8564](https://github.com/moq-dev/moq/pull/8564)), after
Apalache 0.63 could not step the route model itself on one relay pair with
16 GB of heap: ✅ split the model (rejected: a TLAPS proof on the route
model, whose steps break on each model change; deleting the quest and resting
on TLC's bounded runs and the sweeps; a host with far more memory). That PR
proved the claim rules on `RouteClaims.tla`, a model with no paths. This
quest proves the loop rules on a model with no claims, views, or reader
events, after first measuring that Apalache fits it.

- Measure first: one step of a claims-free model from any typed state, on
  the smallest mesh with a loop (the publisher and two relays), then three
  and five relays. If no mesh fits, stop and hand the decision back with the
  measurements, as the claims split did.
- Lessons from `RouteClaims.tla`, which fits five neighbours in about twelve
  minutes:
  - Apalache cannot pick a function of functions: key every per-relay
    variable by a tuple (`<<n, m, k>>`), one level deep.
  - Write each step so TLC can check refinement without enumerating: assign
    a variable from a finite function set (`x' \in [S -> T]`) and constrain
    it, rather than `\E choice : x' = f(choice)`.
  - A ghost the invariant compares pairwise must only grow (`hist' = hist
    \cup {new}`), so a step re-proves only the pairs with the new entry; a
    pairwise rule over values a step copies around never finished.
  - A history keyed by a computed set of revisions made even the first
    state unsolvable; a set of records was fast.
  - Fold one-of-several choices into one transition (`\E change \in
    BOOLEAN`), and check invariants after the step only
    (`search.invariantFilter=1->.*`).
- Unlike the claim rules, the loop rules span several relays (a loop crosses
  them), so one relay's step does not stand for every relay's: the check needs
  the whole mesh, and its size is the bound.
- Start from the path argument: no relay holds or selects a route whose path
  names it, every route in flight extends the path its sender held when it
  sent it, and every withdrawal in flight names a hop whose session closed.
  `SimplePaths` is inductive once the invariant also says a route is held
  only from a node the holder shares a link with
  ([moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)).
- A counterexample to induction starts from a state that may be unreachable.
  If TLC cannot reach it, the invariant is too weak: strengthen it. If it can,
  replay it on `route::tests::Sim` as the skill says before calling it a bug.
- The doubt's timer is a step that may fire at any moment, as the model has
  it. That every hold ends stays an argument in the skill (`Budget`,
  `QuietIsSettled`), not a checked rule.
- Run it through `sh/tla.sh` with an `Apalache:` line in its configs, as
  `RouteClaims.cfg` does.

## Related

- [Route sweeps are checked against the TLA+ model](/quest/m1/route-trace-validation.md) - ties the same spec to the code
