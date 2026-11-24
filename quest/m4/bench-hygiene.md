# [S] Benchmark series stay meaningful

## Goal

Three gaps in the nightly benchmarks are closed: hang's buffered bench has a
`burst` variant with a nonzero warmup, the benchmark report drops sources
that haven't been measured in N days, and the JS slow-group bench's `bytes`
column stays out of series names.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs).

- **Warm burst.** `js/hang/bench/buffered.ts`'s `burst` scenario runs with
  zero warmup, so it never reaches the warming path
  [moq-dev/moq#7523](https://github.com/moq-dev/moq/pull/7523) kept linear.
  Add a variant with a nonzero warmup (and min delay) that would go quadratic
  without #7523's fix.
- **Prune stale sources.** The report already hides a series its source's
  latest run did not measure, and labels a source that stopped running as
  stale with its age (`sh/gh/bench-report.py`, about lines 531 and 545). A
  retired source, such as `bench-ffi`, still keeps its stale section. Drop a
  source not measured in N days, choosing N and recording why.
- **Slow-group names.** `js/hang/bench/slow-group.ts`'s `bytes` column ends
  up in its series names; report it as a value instead, so a change in
  bytes doesn't start a new series.

Public API: none. Wire: none.
