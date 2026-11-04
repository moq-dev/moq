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

Follow-up planning on 2026-11-03 (maintainer) added a load-independent mode to the driver: `just bench-fanout` counts the chat fan-out per delivered group on one pinned thread, through the model alone and through a relay over the in-memory transport, for any list of revisions. [The report](/.github/relay-cpu-bisect.md) attributes each step with it, and the fixes so far take `main`'s model path from 13,300 to 10,126 instructions per group (-23.9%) and the in-process session from 120,948 to 113,133 (-6.5%), at 512 subscribers.

Decided 2026-11-03 (maintainer, on [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)), after the attribution:

1. Costs measured and not cut:
   - ✅ Accept the flat ones (per-reader ranges' remainder, ranged reads, public ingest, compressed tracks, [moq-dev/moq#7052](https://github.com/moq-dev/moq/pull/7052), and bounded reads), recorded with their numbers in the [epic](/quest/m1/release-perf/README.md). Keep [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995)'s and [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542)'s session cost here for a re-measure against this branch.
   - Accept all of them now.
   - Keep investigating all of them.
2. The waiter's list records:
   - ✅ A correctness follow-up at m0, completed in [moq-dev/moq#7789](https://github.com/moq-dev/moq/pull/7789): the metadata/abort and test teardown races are fixed, and waiters remember 16 lists.
   - Leave the limit at 8.
3. `group::Consumer` moves:
   - ✅ A follow-up in this epic: [a read moves a small group consumer](/quest/m1/release-perf/group-consumer-size.md).
   - Leave it.

Remaining:

- Re-measure [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995)'s and [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542)'s session cost against `main` with the probe fast path (about +3.3k and +3.1k per group when each landed; much of it was the probe's walk of the live groups, which the fast path now skips), then fix what is accidental or record what remains in the epic.
- Attribute in-tree qmux ([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)) and receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)), each against its parent, with relay runs on a quiet host: they are QUIC-level, which `just bench-fanout` doesn't reach, and the one attempt on `nixos2` lost the host to other agents after a round.
- Re-pair `main` against [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) on chat and video over both runtimes on a quiet host, then delete this quest.

Measure as the report does. For a model or session change, `just bench-fanout` against the base, which host load doesn't move. For the relay, `BENCH_GENERATOR=21d6da9fc1`, `BENCH_INSTRUCTION_PERIOD=2000003`, `kernel.perf_event_max_sample_rate` raised to 100,000 and restored after, on a quiet host such as `nixos2` (6 cores: relay on 0-3, generator on 4,5,10,11, `BENCH_BUILD_JOBS=2` for its 15 GiB), comparing only cells from one run; instructions per frame fall by up to 20% when other host work makes the relay batch more frames per wakeup.

Public API: none expected. Wire: none.

## Related

- [A read moves a small group consumer](/quest/m1/release-perf/group-consumer-size.md) - found here; the session's largest remaining self cost

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the nightly series that should have caught this growth
- [Performance audit](/quest/m2/audit-perf.md) - counts instructions when bisecting, for the same reason
