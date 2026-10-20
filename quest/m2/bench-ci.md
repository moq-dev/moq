# [M] Nightly benchmark regression report

## Goal

Each Nightly run on `main` compares every measured Rust Criterion benchmark
against the previous nightly's commit, built and run on the same runner, and
publishes the comparison to the git.moq.dev nightly report beside the
[trend site](/quest/m2/bench-site.md). A regression past a loose threshold
fails the job, so the existing Alert workflow posts it to Discord.

Benchmarks never run in the merge queue or on PRs. Rust Criterion targets
only: browser benchmarks stay under `just bench-browser` (see
`bench/README.md`), and relay load stays local under `just bench BASE`.

## Plan

Decided 2026-10-20 (maintainer): benchmarks go into the git.moq.dev report
each night, not the merge queue, because the queue runs only what gates a
merge, and main PRs skip cloud CI since #6038. This drops the per-PR sticky
comment design and its `workflow_run` follow-up.

Researched 2026-09: CodSpeed's instruction counting skips every
`iter_custom` bench (all of `moq-uring`, plus `moq-net` origin and track) and
can't see syscall time. Bencher compares against history recorded on other
VMs, so its thresholds fire on runner noise.

- Paired comparison: build the previous nightly's commit beside `main` on the
  same runner, run the selected targets for both, and compare saved
  Criterion baselines. Only that paired comparison fails the job; the
  cross-run history stays informational on the trend site.
- Extend `bench/run.sh` and `sh/gh/nightly.sh` (`just gh nightly rust-bench`)
  with the paired mode instead of writing a second runner, and publish its
  table through `just gh benchmark-report` into the report.
- The nightly jobs measure in Criterion's quick mode for runtime; the paired
  run needs enough samples for its threshold. Weigh a full run of the
  compared targets against the nightly's time budget.
- Decided 2026-10-16: moq-quic's Criterion targets (#5812) are included.
  They time thread CPU rather than wall time, which already cut most host
  noise; on a shared hybrid-core host, identical builds still swing about 5%
  on small cases and 20-130% on large-working-set ones, so the noise floor
  must come from A/A runs on the CI runner itself, never from a laptop.
- The `moq-uring` socket benches skip loudly when the runner's kernel is too
  old.
- Validate with an A/A run (the same commit twice) and a deliberately slowed
  bench before trusting the signal.
- Document the report and local reproduction in `bench/README.md`.

## Related

- [Benchmark reporting](/bench/README.md) - the existing `bench/run.sh` retains repeated paired rounds
- [Benchmark trend site](/quest/m2/bench-site.md) - where the nightly history is charted
