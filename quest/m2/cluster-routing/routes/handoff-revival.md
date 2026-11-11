# [M] The handoff sweep revives no ended claim

## Goal

`sweep_handoff` in `rs/moq-net/src/route/tests/handoff.rs` reports no revival
over 20,000 scenarios: no reader sees a claim start again after it saw the
claim end, unless it forgot the claim in between. The scenario that revives
one today is a plain test.

## Plan

Found 2026-11-11 by running the sweep past its 150 scenarios: scenario 16316
revives a claim, on `main` at `688312b9eb`, and no earlier one does. It is
the same whichever neighbours the router believes. It has not been traced.
Decided the same day (maintainer, /quest-plan): ✅ plan it.

- Reproduce it: the sweep draws every scenario from one random stream, so run
  the first 16,317. Record the scenario's steps as it runs and remove them
  one at a time while the revival remains, as
  [the false END](/quest/m2/cluster-routing/routes/widen-false-end.md) was
  shrunk.
- Decide whether it is a revival the contract forbids or one it excuses: a
  replay after a collection or a restart is allowed only until the
  withdrawal behind it lands
  ([epic](/quest/m2/cluster-routing/routes/README.md), handoff contract). If
  the fixture's `revivals` count misses an excuse, fix the count and say so.
- Otherwise fix the cause and keep the shrunk case as a test.
- If the cause is in what
  [the route per scope](/quest/m2/cluster-routing/routes/scope.md) deletes,
  pin it as a `residual_` test naming that quest instead of fixing it twice.

Public API: none. Wire: none.

## Related

- [Widening an interest ends no live claim](/quest/m2/cluster-routing/routes/widen-false-end.md) - found the same way, and adds the way to run more scenarios
