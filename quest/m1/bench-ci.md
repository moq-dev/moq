# [S] Calibrate the paired nightly benchmarks

## Goal

The paired Rust Criterion comparison each Nightly runs (see
[Paired comparison](/bench/README.md#paired-comparison)) fails only on real
regressions in every paired group: A/A runs on the hosted ARM runners hold its
threshold, confirmation budget, and round count for `rust-bench`
and `rust-relay` too, and the rounds stop agreeing on slowdowns in code that
did not change.

## Waiting

- moq-dev/moq: once [moq-dev/moq#7940](https://github.com/moq-dev/moq/pull/7940)
  merges, an agent may dispatch exactly one A/A Nightly run on `main`, with
  `bench-base` set to main's head, under the authorization recorded below.
  Check: that run's `bench-rust` and `bench-rust-relay` artifacts each hold
  `paired.json` at the artifact root (the gate's input), rather than only
  partial `runs/run.*/paired.json` files.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): authorize exactly one agent
dispatch of an A/A Nightly run on `main` once
[moq-dev/moq#7940](https://github.com/moq-dev/moq/pull/7940) merges:
`gh workflow run nightly.yml --ref main -f bench-base=<main's head>`.
Check that `bench-rust` and `bench-rust-relay` each produce an artifact-root
`paired.json`. A dispatch cancels any in-progress Nightly through its
concurrency group.

Calibrated 2026-11-05 for the shared, supplemental (`rust-extra`) and io_uring
groups; `bench/README.md` records the numbers. Threshold 10%, 20
confirmations, and 3 rounds stay as they were:

- A/A runs [41921320690](https://github.com/moq-dev/moq/actions/runs/41921320690)
  and [41933525520](https://github.com/moq-dev/moq/actions/runs/41933525520)
  (the first cancelled by Nightly's concurrency group after its bench
  artifacts uploaded) confirmed no regression. Screen crossings of 10% either
  way: shared 18 and 26 of 716, supplemental 10 and 15 of 460, io_uring 1 and
  1 of 41; at most 17 slower in one group, under the 20 confirmations. The
  largest median among round sets that all agreed on a slowdown was +5.6%.
- Slowed-bench run
  [41979277343](https://github.com/moq-dev/moq/actions/runs/41979277343)
  compared `main` at `c150456928` against a base that collects 192 of
  `pattern-algebra/collect-literal/256`'s 256 literals (branch
  `bench-ci/slowed-base`, deleted). The shared job failed at its gate with
  that case alone: screen +35.7%, rounds +34.6, +35.1, +35.2%. The
  supplemental and io_uring groups passed.
- In that run, the rounds rejected the three `stats/tick/*_10pct` cases the
  screen flagged, but each one's rounds agreed on a slowdown, at medians of
  +9.3 to +9.7%, and every `100pct` case screened +6 to +9%, although `moq-stats` was source-identical on both sides.
  The same cases sat within 6% either way in both A/A runs. A local build of
  `producer_tick` at both commits, in separate trees and target directories,
  gives byte-identical `.text`, `.rodata` and `.data.rel.ro`, so the measured
  code is the same. That is the stable bias
  [moq-dev/moq#7139](https://github.com/moq-dev/moq/pull/7139) found at +23%
  and +46%, and it sits 0.3 points under the threshold. Find its source on the
  ARM runner (for example, diff the two `producer_tick` builds there and rerun
  the rounds against one binary in both slots), then fix it in `bench/run.sh`.
  Lowering the threshold waits on that fix.
- The shared, supplemental and io_uring legs took 19 to 44 minutes of their
  hour across the three runs.

Decided 2026-11-05 (agent, recorded in
[moq-dev/moq#7928](https://github.com/moq-dev/moq/pull/7928)): `bench/run.sh`
does not pin glibc's malloc thresholds for every Criterion target. The largest
A/A screen outliers (`moq-mux/containers`' `ts_si_*`, +35 to +49% in both A/A
runs) were each rejected by rounds within a few percent, so a heap mode flip
costs confirmation budget but no false alarm, and a harness-wide pin would
shift every trend line and measure an allocator production does not run. A
bench whose rounds show a mode flip pins its own heap, as `ts_schedule` does.

Decided 2026-10-23 (maintainer), when the paired mode landed:

- The gate fails Nightly from day one rather than reporting only until A/A
  runs: Nightly already alerts for unrelated legs, so a false paired alarm adds
  no new channel, and this quest tunes it.
- A target the base cannot build or run fails the gate rather than passing
  uncompared; it clears by itself the next night.

Rejected hosts (decided 2026-10-22, maintainer): the merge queue runner, the
moq.pro load nodes, and a dedicated bare-metal host; revisit the last only if
A/A runs show the hosted noise floor hides regressions worth catching.

Decided 2026-10-28 (maintainer): CodSpeed's CPU simulation runs on every
`main` commit as an extra signal beside this gate rather than a replacement
(see [CodSpeed](/bench/README.md#codspeed)). It covers only the
single-threaded benches without I/O, so this calibration still decides the
paired gate.

## Related

- [Benchmark reporting](/bench/README.md) - the paired comparison and the trend table
