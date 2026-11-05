# [M] Find and fix the relay's per-frame CPU growth

## Goal

The commits that raised the relay's instructions per delivered frame between [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95) ([moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) as merged) and `main` are found, each rise is attributed to a cause, and the avoidable costs are fixed. A rise that pays for a deliberate feature is accepted with its reason in the [release-perf epic](/quest/m1/release-perf/README.md).

## Plan

Found by [moq-dev/moq#7505](https://github.com/moq-dev/moq/pull/7505). Decided 2026-11-01 (maintainer): m1, in this epic, since `main` must not reach the cut carrying accidental per-frame cost.

[The report](/.github/relay-cpu-bisect.md) has every step, its cause, and its evidence. [moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605) bisected the rise, fixed the accidental `Cursor::probe` work, and recorded the costs the maintainer accepted on 2026-11-03 in the epic. [moq-dev/moq#7874](https://github.com/moq-dev/moq/pull/7874) finished the remaining measurements:

- [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995)'s and [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542)'s session costs were never the probe's judgement, so the fast path left both. One branch per live walk took back 891 per group; the rest is accepted in the epic.
- In-tree qmux ([moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231)) and receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)) cost nothing measurable on the relay. The step between them is public ingest ([moq-dev/moq#6111](https://github.com/moq-dev/moq/pull/6111)): +10.5k per frame on chat over io\_uring, in each viewer's frame read.
- `main` against the old endpoint, on the quiet test host: chat 1.28x on io\_uring and 1.10x on the tokio workers, video 1.12x and 0.89x.

What remains is public ingest's relay cost and the `group_relay_frames` item below. The epic accepted public ingest on 2026-11-03 at its model cost (+200 per group), which never reads a frame.

Decided 2026-11-05 (maintainer, /quest-unblock): keep cutting public ingest's relay cost ([moq-dev/moq#6111](https://github.com/moq-dev/moq/pull/6111), +10.5k of `main`'s +30.8k per frame on chat over io\_uring). Count `main`'s per-viewer read with `just bench-fanout` session and callgrind, fix what a live read pays for nothing, and record the rest in the epic. The listed lead is `Consumer::poll_expiry` calling `Recover::poll_budget` before `is_expired`; skipping it needs the mirrored-preference sync proven redundant. Not chosen: accepting the cost, or handing it to the [performance audit](/quest/m2/audit-perf.md). Also remove, or make crate-private, any public API on the relay read paths this work touches that no consumer outside its own crate uses.

Measure as the report does. For a model or session change, `just bench-fanout` against the base, which host load doesn't move; the session scenario reads every frame, so it reaches public ingest's read path. For the relay, `BENCH_GENERATOR=21d6da9fc1`, `BENCH_INSTRUCTION_PERIOD=2000003`, `kernel.perf_event_max_sample_rate` raised to 100,000 and restored after, on a quiet host such as `nixos2` (relay on CPUs 0-3, generator on 4,5,10,11, `BENCH_BUILD_JOBS=2`), comparing only cells from one run.

Added 2026-11-04 (maintainer) from [fastest-reader pacing](https://github.com/moq-dev/moq/pull/7664)'s comparison on a loaded host: `main` at `f37d964` reads `group_relay_frames` about 15% slower than `e7a5c10`. Pacing ([moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388)) explains about 4% of that, and [its own quest](/quest/m1/release-perf/pacing-report.md) trims it; attribute the remaining ~10% here, on a quiet host.

Public API: none expected. Wire: none.

## Related

- [A waiter's record limit changes no behavior](/quest/m0/kio-waiter-records.md) - found here; the session's waiter registration cost waits on it
- [Benchmark sessions can't overlap on a host](/quest/m0/bench-host-lock.md) - the quiet-host runs here were voided twice by other sessions' work
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - the nightly series that should have caught this growth
- [Performance audit](/quest/m2/audit-perf.md) - counts instructions when bisecting, for the same reason
