# [S] Calibrate the paired nightly benchmarks

## Goal

The paired Rust Criterion comparison each Nightly runs (see
[Paired comparison](/bench/README.md#paired-comparison)) fails only on real
regressions: its threshold, confirmation budget, and round count come from A/A
runs on the hosted ARM runners, and a deliberately slowed bench fails the job.

## Plan

Waits on maintainer-dispatched Nightly runs on `main` after the paired mode
lands; agents never dispatch Nightly.

- A/A: dispatch Nightly on `main` with `bench-base` set to the run's own
  commit, at least twice. Read each paired group's `paired.json` (the
  `bench-rust*` artifacts, or `bench/paired.json` on the site): the spread of
  screen deltas sets the screen threshold, and any confirmed regression in an
  A/A run means the rounds or threshold are too loose.
- Slowed bench: dispatch with `bench-base` on a branch commit that speeds up
  one cheap bench (so tonight's `main` reads as slower), and check the job fails
  with that case alone.
- Tune `THRESHOLD_PCT`, `CONFIRMATIONS` (`bench/compare.py`) and the round count
  (`BENCH_RUNTIME_ROUNDS`, default three) from what the runs show, and
  update the README's numbers. Bimodal series (about 50% up and back across
  nights) are the expected false positives; three alternating rounds pass one
  only when all three land on the slow mode.
- Check the paired legs' durations against their hour; the base build relies
  on the mbx cache for the previous night's artifacts, and a cold one roughly
  doubles each leg's build time.

- Decide whether `bench/run.sh` pins glibc's malloc thresholds for every
  Criterion target, as [moq-dev/moq#6727](https://github.com/moq-dev/moq/pull/6727)
  did for `ts_schedule`: some small paired deltas may be the same heap mode flip.

Decided 2026-10-23 (maintainer), when the paired mode landed:

- The gate fails Nightly from day one rather than reporting only until A/A
  runs: Nightly already alerts for unrelated legs, so a false paired alarm adds
  no new channel, and this quest tunes it.
- Start at a 10% threshold, 20 confirmations per group, and 3 rounds; 10%
  matches the trend table's floor, and three-round sign agreement filters
  bimodal cases.
- `rust-shared` splits out of `rust-bench`, restarting the `rust/shared/*`
  trend history (three nights lost), since `rust-bench` paired would exceed
  its hour unsplit.
- A target the base cannot build or run fails the gate rather than passing
  uncompared; it clears by itself the next night.

Rejected hosts (decided 2026-10-22, maintainer): the merge queue runner, the
moq.pro load nodes, and a dedicated bare-metal host; revisit the last only if
A/A runs show the hosted noise floor hides regressions worth catching.

Decided 2026-10-27 (maintainer): if A/A runs show that, first try
[CodSpeed](https://codspeed.io)'s CPU-simulation instrument on the pure-CPU
Criterion microbenchmarks, before a bare-metal host. It counts instructions on
the hosted runners, so its noise is near zero, and it is free for open source.
It does not measure syscalls, I/O, or contention, so the relay load, uring,
transport, and session sweeps stay on the paired hosted comparison.

The A/A calibration includes moq-quic's windowed `benches/streams.rs` cases:
on a shared host the unchanged base alone varied 17 to 43% between
back-to-back runs ([moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)).

Confirmed regressions with no extra work behind them, found by
[moq-dev/moq#7139](https://github.com/moq-dev/moq/pull/7139): every round of
`moq-net/group_relay_frames/plain/*` (+23%) and `moq-net/origin/serve_idle/3000r`
(+46%) agreed, yet callgrind counts the same instructions per frame read and per
poll on both builds. A paired round reruns the same two binaries, so a code or
heap layout shift between them reads as a stable regression that rounds cannot
filter. `codec_messages/*/lite` (+31%, +68%) moved only because a new bench group
in the same binary changed what LTO inlines.

## Related

- [Benchmark reporting](/bench/README.md) - the paired comparison and the trend table
