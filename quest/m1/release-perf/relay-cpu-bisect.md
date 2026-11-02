# [M] Find and fix the relay's per-frame CPU growth since #5926

## Goal

The commits that raised the relay's instructions per delivered frame between [`cdff3c8f95`](https://github.com/moq-dev/moq/commit/cdff3c8f95) ([moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) as merged) and `main` are found, each rise is attributed to a cause, and the avoidable costs are fixed. A rise that pays for a deliberate feature is accepted with its reason in the [release-perf epic](/quest/m1/release-perf/README.md).

## Plan

Found by [moq-dev/moq#7505](https://github.com/moq-dev/moq/pull/7505), which profiled the io_uring clock read. At `main` [`30f23c3f8a`](https://github.com/moq-dev/moq/commit/30f23c3f8a), relay instructions per delivered frame had risen 25-56% on io_uring since `cdff3c8f95` and 18-38% on the tokio workers since `1dd9b83d1`, with the same delivery:

| workload | runtime | before, kinstr/frame | `30f23c3f8a`, kinstr/frame |
|---|---|---|---|
| chat | io_uring | 86.3 [84.5-87.4] at `cdff3c8f95` | 134.8 [128.5-151.3] |
| video | io_uring | 108.7 [106.6-116.2] at `cdff3c8f95` | 136.1 [132.6-152.8] |
| chat | tokio workers | 104.0 [101.4-105.0] at `1dd9b83d1` | 143.1 [127.5-205.3] |
| video | tokio workers | 141.6 [139.3-148.3] at `1dd9b83d1` | 167.1 [164.9-173.6] |

Medians of 5 interleaved reps, min-max in brackets. The tokio rows start at [`1dd9b83d1`](https://github.com/moq-dev/moq/commit/1dd9b83d19), #5926's base, since #7505 measured the tokio workers only there and at `main`. Both runtimes moved, so at least one cause is shared code (moq-net's model, kio, or the session layer). That does not rule out a further io_uring-only cost, such as the clock read #7505 profiled. The tokio range also spans #5926 itself, so the two rises can't be subtracted to find one.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m1, in this epic, since `main` must not reach the cut carrying accidental per-frame cost.

- Measure as #7505 did: `--profile profiling` builds with frame pointers and `moq-relay/io-uring,moq-bench/uring`, one target directory per revision, `perf stat` `cpu_core` instructions (user and kernel) over the steady window, divided by the frames the generator's 500 ms reports count over the same window. 4 io_uring workers, not 1, so chat does not saturate a worker. Pin the relay and generator cores so a loaded host still reads instruction deltas.
- Bisect `cdff3c8f95..main` on chat over io_uring first (the largest rise and the tightest ranges), with a step size that keeps each build's cost bounded, then confirm the culprits on video and on the tokio workers. A rise that shows on io_uring but not on the tokio workers is attributed on its own, not folded into a shared culprit.
- Attribute each step with `perf record` at a fixed period, as #7505 did, before deciding fix or accept. Record accepted costs in the epic's Plan with the feature they pay for.
- Commit the driver this needs as a deterministic mode of `just bench-profile` or `just bench-runtime` (pinned cores, instructions per delivered frame, fixed-period samples) instead of another throwaway script; #7505's driver on `bench/relay.sh` was not committed.
- Implementation checkpoint in [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605): `just bench-profile instructions` owns disjoint relay/generator affinities, acknowledged PMU windows matched to interpolated 500 ms delivery reports, alternating revision pairs, and fixed-period source profiles. Missing or multiplexed counters, lost/throttled samples, and unbracketed delivery refuse the result. Historical profiling builds explicitly load only their own Cargo configuration, avoiding ancestor alias conflicts in nested scratch checkouts.
- The bisect, attribution, and resulting fixes remain blocked on the profiling host's sample-rate configuration. With `kernel.perf_event_max_sample_rate=1000`, both a 1,000,003-instruction current-tree capture and a 10,000,019-instruction historical capture throttled. The historical run had 957 instruction samples and 957 THROTTLE records. Increasing the period did not produce complete fixed-period attribution. Decide in the foreground whether to configure a higher host sample-rate limit temporarily and restore it afterward, or use another profiling host. No host sysctl was changed, and no feature cost has been accepted or optimization attributed. Re-run both endpoints with identical capture settings before bisecting; then confirm each measured step on video and Tokio workers. Keep the separately owned [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) out of this fix.
- [Fanout lock contention](/quest/m1/release-perf/fanout-lock.md) covers the bimodal futex cost on the fanout workload; don't fix it twice. A bisect step that lands on the same lock goes there.

Public API: none expected. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the nightly series that should have caught this growth
- [Performance audit](/quest/m2/audit-perf.md) - counts instructions when bisecting, for the same reason
