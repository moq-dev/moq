# [S] Share Python check and test preparation

## Goal

A CI pass that selects Python prepares its editable bindings and environment
once for both checks and tests. Direct `just py check` and `just py test`
remain self-contained and validate the current checkout.

## Plan

Decided 2026-10-23: remove repeated Python preparation before attempting
parallel execution of whole binding suites. Keep existing coverage and the
`testing,openh264` feature contract.

- `py/justfile` currently synchronizes the environment and runs `_develop`
  in both recipes. `sh/dispatch.sh` separates them with other binding checks
  that also build through Cargo.
- In [queue run 39996587979](https://github.com/moq-dev/moq/actions/runs/39996587979),
  the second preparation rebuilt the UniFFI generator for 43.70 seconds,
  in addition to native build and installation work. Repeated compilation
  is observed; the exact cache-invalidation cause is not yet established.
- Share preparation within one invocation rather than trusting an old
  environment or a persistent success marker. Keep standalone recipes safe
  after Rust, binding, feature, or dependency changes, and stop on preparation
  failure. Prefer the smallest recipe/dispatch refactor that achieves this.
- Other languages use different features and build environments. This quest
  does not unify every native artifact or run suites concurrently against
  shared mutable Cargo outputs.
- Verify one preparation in the combined pass, standalone checks and tests
  from a fresh environment, and rebuilds after relevant input changes.
  Measure combined Python and full selected-gate time before and after on
  matched inputs and cache state; report build counts and memory use. The
  observed 44 seconds is a candidate saving, not a guaranteed improvement.
