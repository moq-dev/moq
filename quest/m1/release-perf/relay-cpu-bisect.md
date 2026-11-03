# [M] Find and fix the relay's per-frame CPU growth

## Goal

The commits that raised the relay's instructions per delivered frame between [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) ([moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) as merged) and `main` are found, each rise is attributed to a cause, and the avoidable costs are fixed. A rise that pays for a deliberate feature is accepted with its reason in the [release-perf epic](/quest/m1/release-perf/README.md).

## Plan

Found by [moq-dev/moq#7505](https://github.com/moq-dev/moq/pull/7505). Decided 2026-11-01 (maintainer): m1, in this epic, since `main` must not reach the cut carrying accidental per-frame cost.

Bisected in [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605), which committed the driver (`just bench-profile instructions`, several rotated bases per run, a pinned `BENCH_GENERATOR`, per-cell host load) and [the report](/.github/relay-cpu-bisect.md). On chat over io_uring, the relay peaked at 1.82x the old endpoint and `main` is at 1.26x:

- [moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224)'s `Reader::learn_end` polling (+26k instructions per frame) and the track lock contention after [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) were accidental. [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532) and [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) already removed both from `main`.
- `main` against the old endpoint: chat 1.26x on io_uring and 1.31x on the tokio workers, video 1.14x on io_uring and 0.96x on the tokio workers. Chat's growth is shared code, in the same functions on both runtimes.

What remains is set by the maintainer's decisions in [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605):

- `Cursor::probe` and `Misses::judge` ([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)'s fills for misses) cost about 8.6k per frame on every peek, taking the shared track lock and cloning its fills even for a live reader that never misses a group. Either a fast path that skips them while nothing is missable, or acceptance as the cost of relay backfill.
- The cursor's window, drift, and live-edge reads (about 9-11k, from [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929)'s per-reader ranges and time anchors) and the 2-4k feature steps before [e84ffb3de3](https://github.com/moq-dev/moq/commit/e84ffb3de3139f7245bed5697754fd6add9a022e) (compressed tracks, ranged frame reads, qmux in tree, signed priorities, public ingest, receive leases) are feature costs to accept or not.
- kio's task and waiter growth (about 11k: `Tasks::poll_each`, `WaiterList::register`, `Waiters::release`) and the lite publisher's serving and close polling (about 5-7k) have no single feature behind them yet.

Then record each accepted cost and its feature in the epic's Plan, re-pair `main` against [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) on chat and video over both runtimes, and delete this quest.

Measure as the report does: `BENCH_GENERATOR=21d6da9fc1`, `BENCH_INSTRUCTION_PERIOD=2000003`, `kernel.perf_event_max_sample_rate` raised to 100,000 for the gated run and restored after, and compare only cells from one run at low host load. Instructions per frame fall by up to 20% when other host work makes the relay batch more frames per wakeup, so cross-session absolutes don't compare.

Public API: none expected. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the nightly series that should have caught this growth
- [Performance audit](/quest/m2/audit-perf.md) - counts instructions when bisecting, for the same reason
