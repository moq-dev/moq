# [M] Nightly benchmark regression report

## Goal

Each Nightly run on `main` compares every measured Rust Criterion benchmark
against the previous nightly's commit, built and run on the same runner, and
publishes the comparison to the git.moq.dev nightly report beside the
[regression table](https://git.moq.dev/bench/). A regression past a loose threshold
fails the job, so the existing Alert workflow posts it to Discord.

Benchmarks never run in the merge queue or on PRs. Rust Criterion targets
only: browser benchmarks stay under `just bench-browser` (see
`bench/README.md`), and relay load stays local under `just bench BASE`.

## Plan

Decided 2026-10-20 (maintainer): benchmarks go into the git.moq.dev report
each night, not the merge queue, because the queue runs only what gates a
merge, and main PRs skip cloud CI since #6038. This drops the per-PR sticky
comment design and its `workflow_run` follow-up.

Decided 2026-10-22 (maintainer): the paired run stays on the hosted ARM
runners. Moved from m2 to m1 so the [performance audit](/quest/m2/audit-perf.md)
reads paired flags instead of raw history. Rejected hosts:

- The merge queue runner (`nixos-moq-ci`) is an nspawn container on a
  desktop with `CPUWeight=10`, a floating 8-core quota, and the `powersave`
  governor beside unrelated GUI, simulation, and agent load. A benchmark
  there is the first workload starved, and nightly's benchmark jobs would
  serialize on the one queue runner and hold merges for hours.
- moq.pro's two load nodes are shared 4-vCore, 8 GB OVH VPSes with measured
  steal, and moq.pro decided they run nothing but the load generator.
- A dedicated bare-metal bench host would be the most stable but costs money
  and adds a persistent exec surface for a public repository. Revisit it only
  if A/A runs show the hosted noise floor hides regressions worth catching.

The paired run removes runner-to-runner variance; what remains is noise
within one VM, which alternating rounds and A/A-derived thresholds handle.
The first three nights of hosted history (2026-10-20 to 22, 95-129 commits
apart) put the median night-to-night swing at about 4%, with 13% of Rust
series moving more than 10%, many up about 50% and back the next night.
Those nights mix code changes with noise, so they bound it rather than
measure it; the A/A run measures it.

Rust Criterion only (decided 2026-10-22): `bench-ffi` stays history-only,
since its JVM and Dart rows are the noisiest (Kotlin about 18% a night),
and JS on bun only stands in for the browser.

Researched 2026-09: CodSpeed's instruction counting skips every
`iter_custom` bench (all of `moq-uring`, plus `moq-net` origin and track) and
can't see syscall time. Bencher compares against history recorded on other
VMs, so its thresholds fire on runner noise.

- Paired comparison: build the previous nightly's commit beside `main` on the
  same runner, run the selected targets for both, and compare saved
  Criterion baselines. Only that paired comparison fails the job; the
  cross-run history stays informational on the regression table.
- Extend `bench/run.sh` and `sh/gh/nightly.sh` (`just gh nightly rust-bench`,
  `rust-extra`, and `rust-uring`) with the paired mode instead of writing a second runner, and publish its
  table through `just gh benchmark-report` into the report. `nightly.sh`
  exits on a failure before it publishes today; publish first, so the table
  exists when the regression alert fires.
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
