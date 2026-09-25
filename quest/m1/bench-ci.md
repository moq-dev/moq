# [M] Benchmark regressions in CI

## Goal

A pull request that touches a benched Rust crate, or anything it depends on,
gets a sticky comment comparing its Criterion benchmarks against the base. The
comment never fails the PR. A nightly run on `main` keeps a history of every
benchmark and fails when one crosses a loose regression threshold, so the
existing Alert workflow posts it to Discord.

Rust Criterion targets only. Browser and JS benchmarks stay with
[Browser benchmarks](/quest/m1/browser-benchmarks.md), and relay load stays
local under `just bench BASE`.

## Plan

Researched 2026-09: CodSpeed's instruction counting skips every
`iter_custom` bench (all of `moq-uring`, plus `moq-net` origin and track) and
can't see syscall time. Bencher compares a PR against history recorded on other
VMs, so its thresholds fire on runner noise. The chosen shape, free and without
a GitHub App:

- PR: one job builds base and head on the same runner and runs only the
  selected targets, saving and comparing Criterion baselines. Selection is the
  changed crates plus their dependents, from the impact map that
  [Thin justfiles](/quest/m1/tooling/justfiles.md) lands. No selected bench
  means no job. Extend `bench/run.sh` with a Criterion-only, crate-scoped mode
  behind a recipe instead of writing a second runner. Hosted runners vary by
  about 3%, so the comment highlights only changes Criterion calls
  significant and beyond a noise floor you choose from A/A runs. Fork PRs
  have a read-only token, so posting needs a `workflow_run` follow-up.
- Nightly: run every target on one fixed runner type and record the results
  with
  [github-action-benchmark](https://github.com/benchmark-action/github-action-benchmark)
  on a data branch. The `moq-uring` socket benches run only here, and skip
  loudly when the runner's kernel is too old. Add the workflow's name to
  `alert.yml` if it isn't `Nightly`.
- Validate with an A/A run (the same commit twice) and a deliberately slowed
  bench before trusting either signal.
- Document the comment, the trend, and local reproduction in
  `bench/README.md`.

## Required

- [Thin justfiles](/quest/m1/tooling/justfiles.md) - owns the diff-to-crate impact map the PR job reuses

## Related

- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - extends the same `bench/run.sh` with repeated paired rounds
- [Sans-IO session bench](/quest/m1/bench-session.md) - a low-noise end-to-end bench this job picks up
- [Bench coverage](/quest/m1/bench-coverage.md) - more targets for this job to track
