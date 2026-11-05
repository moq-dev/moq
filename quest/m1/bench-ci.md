# [S] Calibrate the paired nightly benchmarks

## Goal

The paired Rust Criterion comparison each Nightly runs (see
[Paired comparison](/bench/README.md#paired-comparison)) fails only on real
regressions in every paired group: `rust-bench` finishes its paired leg within
its hour and A/A runs on the hosted ARM runners hold its threshold,
confirmation budget, and round count, and separate builds of unchanged code
stop measuring as a stable slowdown.

## Plan

Calibrated 2026-11-05 for the shared, supplemental (`rust-extra`), io_uring
and `rust-relay` groups; `bench/README.md` records the numbers. Threshold 10%,
20 confirmations, and 3 rounds stay as they were:

- A/A runs [41921320690](https://github.com/moq-dev/moq/actions/runs/41921320690)
  and [41933525520](https://github.com/moq-dev/moq/actions/runs/41933525520)
  (the first cancelled by Nightly's concurrency group after its bench
  artifacts uploaded) confirmed no regression. Screen crossings of 10% either
  way: shared 18 and 26 of 716, supplemental 10 and 15 of 460, io_uring 1 and
  1 of 41; at most 17 slower in one group, under the 20 confirmations. The
  largest median among round sets that all agreed on a slowdown was +5.6%.
- A/A runs [42102235726](https://github.com/moq-dev/moq/actions/runs/42102235726)
  (`main` at `62ae716265`) and
  [42119312487](https://github.com/moq-dev/moq/actions/runs/42119312487)
  (`83d0bc46ba`), dispatched after
  [moq-dev/moq#7940](https://github.com/moq-dev/moq/pull/7940) merged, each
  verified with `head_sha` equal to `bench-base`. Crossings: shared 20 and 19
  of 728, supplemental 7 and 8 of 514 and 532, io_uring 3 and 2 of 41, relay
  15 and 37 of 410; at most 17 slower in one group. Relay confirmed nothing in
  either run and is calibrated. The second run confirmed nothing. The first
  failed the shared gate on `wire_decode/padded/packets_64`: screen +10.3%,
  rounds +11.9, +12.9, +12.1%, with `padded/packets_1024` at +7.1%. The same
  case read +2.1% in the second run and +5.4, -0.8 and +6.2% in the three
  earlier runs.
- Slowed-bench run
  [41979277343](https://github.com/moq-dev/moq/actions/runs/41979277343)
  compared `main` at `c150456928` against a base that collects 192 of
  `pattern-algebra/collect-literal/256`'s 256 literals (branch
  `bench-ci/slowed-base`, deleted). The shared job failed at its gate with
  that case alone: screen +35.7%, rounds +34.6, +35.1, +35.2%. The
  supplemental and io_uring groups passed. Every group runs the same gate
  step, so no other group needs its own slowed run.
- In that run, the rounds rejected the three `stats/tick/*_10pct` cases the
  screen flagged, but each one's rounds agreed on a slowdown, at medians of
  +9.3 to +9.7%, and every `100pct` case screened +6 to +9%, although
  `moq-stats` was source-identical on both sides. The same cases sat within 6%
  either way in both earlier A/A runs. A local build of `producer_tick` at both
  commits, in separate trees and target directories, gives byte-identical
  `.text`, `.rodata` and `.data.rel.ro`.

Find the source of that bias, then fix it in `bench/run.sh`. It moves between
runs rather than following the source: an A/A run builds the same commit on
both sides, yet `wire_decode/padded/packets_64` agreed on +12% in one run and
not the next. A paired round reruns the same two binaries, so rounds cannot
filter it. Rerun the rounds against one binary in both slots on the ARM
runner to tell build differences from run placement. Lowering the threshold
waits on that fix.

`rust-bench` is not calibrated: its leg hit its 60-minute timeout in both
runs, before uploading an artifact. Building both sides took 16 to 18
minutes. The screen and rounds then ran past 40 minutes, because confirmation
rounds reran benches whose untimed work dominates their wall time. A round
side meant to take about 3 seconds took 2 minutes for
`interests/covered/1000w_1000s` and 8 for
`publisher_covered_before_start/wake/8192`: each builds its fixture in the
`bench_function` closure outside `b.iter`, and Criterion calls that closure
once per sample. `track_evict_fanout/512r/8` took about a minute, because its
untimed reads run inside every iteration. Make those benches pay their setup
once per benchmark, and bound the rounds by time if the leg still does not
fit. Its A/A run then needs a maintainer-authorized Nightly dispatch.

A dispatch cancels any in-progress Nightly through its concurrency group, and
the 07:00 UTC scheduled run can cancel a dispatch. Dispatch clear of that
window, and verify that the run's `head_sha` equals `bench-base` before
treating it as A/A evidence.

Retain the earlier bias cases when reading `rust-bench`'s A/A results:
[moq-dev/moq#7139](https://github.com/moq-dev/moq/pull/7139) found
`moq-net/group_relay_frames/plain/*` (+23%) and
`moq-net/origin/serve_idle/3000r` (+46%) agreed in every round despite equal
callgrind instruction counts. `codec_messages/*/lite` (+31%, +68%) changed
because a new bench group shifted LTO inlining.
[moq-dev/moq#7253](https://github.com/moq-dev/moq/pull/7253) found
`server_admission/workers=4/0` (+10%) had identical machine code apart from
addresses. Include moq-quic's windowed `benches/streams.rs` cases too: an
unchanged base varied 17 to 43% between back-to-back runs on a shared host
([moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)).

Decided 2026-11-05 (agent, recorded in
[moq-dev/moq#7928](https://github.com/moq-dev/moq/pull/7928)): the threshold
stays at 10% despite the A/A false alarm at +12.1%. The 2026-10-23 decision
already accepts false paired alarms while this quest tunes the gate. Raising
the threshold would hide the bias this quest fixes, along with real
regressions of 10 to 15%.

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
`main` push that touches Rust, Cargo, or Nix files, as an extra signal beside
this gate rather than a replacement
(see [CodSpeed](/bench/README.md#codspeed)). It covers only the
single-threaded benches without I/O, so this calibration still decides the
paired gate.

## Related

- [Benchmark reporting](/bench/README.md) - the paired comparison and the trend table
