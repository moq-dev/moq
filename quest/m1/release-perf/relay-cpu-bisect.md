# [M] Find and fix the relay's per-frame CPU growth

## Goal

The commits that raised the relay's instructions per delivered frame between [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) ([moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) as merged) and `main` are found, each rise is attributed to a cause, and the avoidable costs are fixed. A rise that pays for a deliberate feature is accepted with its reason in the [release-perf epic](/quest/m1/release-perf/README.md).

## Plan

Found by [moq-dev/moq#7505](https://github.com/moq-dev/moq/pull/7505). Decided 2026-11-01 (maintainer): m1, in this epic, since `main` must not reach the cut carrying accidental per-frame cost.

Bisected in [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605), which committed the driver (`just bench-profile instructions`, several rotated bases per run, a pinned `BENCH_GENERATOR`, per-cell host load) and [the report](/.github/relay-cpu-bisect.md). On chat over io_uring, the relay peaked at 1.82x the old endpoint and `main` is at 1.26x:

- [moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224)'s `Reader::learn_end` polling (+26k instructions per frame) and the track lock contention after [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171) were accidental. [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532) and [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) already removed both from `main`.
- `main` against the old endpoint: chat 1.26x on io_uring and 1.31x on the tokio workers, video 1.14x on io_uring and 0.96x on the tokio workers. Chat's growth is shared code, in the same functions on both runtimes.

Decided 2026-11-03 (maintainer, on [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)):

1. `Cursor::probe` on every peek ([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171), about 8.6k instructions per frame):
   - ✅ Add a fast path in this quest that skips the work while nothing is missable.
   - Accept it as the cost of relay backfill.
2. The cursor's window, drift, and live-edge reads ([moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929), about 9-11k) and the 2-4k steps from compressed tracks, ranged reads, in-tree qmux, signed priorities, public ingest, and receive leases:
   - Accept them as feature costs.
   - ✅ Investigate each: profile each step's added instructions, fix what can be cut without changing behavior, and attribute what can't with the evidence.
3. kio's task and waiter growth (about 11k) and the lite publisher's serving and close polling (about 6k):
   - ✅ Profile them in this quest, then fix or attribute them before closing.
   - Hand them to the [performance audit](/quest/m2/audit-perf.md).

`Cursor::probe` is the relay's check for groups a reader has missed. Each time a subscriber asks for its next group, the probe looks at what arrived on the track since its last look and decides whether any group the reader still wants has left the cache without the live feed delivering it (evicted, skipped, or from before the feed started), and asks upstream to fill those. A live reader that takes each group as it arrives never has such a gap, yet it paid for the whole check on every group: locking the subscription and the track's shared state, cloning the track's fills, walking the arrival ledger, checking each new group for holes, and running the ask-and-answer loop. On top of that, each time the cache aged out an old group, the eviction landed with every reader, all of which had already taken it, and forced each one to re-run the full check.

Fixes measure each change exactly with a deterministic replay of moq-net's `track_fanout_group` bench (one finished group, every subscriber takes it and parks), counting user instructions per group per subscriber. The same harness builds against historical revisions, so the model's share of the growth is bisected exactly; the session and QUIC layers are attributed from relay cells.

Remaining: finish the per-step attribution of decision 2 and the profiling of decision 3, land what can be cut, record each attributed cost and its feature in the epic's Plan, re-pair `main` against [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) on chat and video over both runtimes, and delete this quest.

Measure as the report does: `BENCH_GENERATOR=21d6da9fc1`, `BENCH_INSTRUCTION_PERIOD=2000003`, `kernel.perf_event_max_sample_rate` raised to 100,000 for the gated run and restored after, and compare only cells from one run at low host load. Instructions per frame fall by up to 20% when other host work makes the relay batch more frames per wakeup, so cross-session absolutes don't compare.

Public API: none expected. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the nightly series that should have caught this growth
- [Performance audit](/quest/m2/audit-perf.md) - counts instructions when bisecting, for the same reason
