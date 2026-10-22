# Perf questline

## Goal

Reduce relay CPU per session, raise the per-worker throughput ceiling, and
hold tail latency on the thread-per-core stack by eliminating measured
hot-path costs: redundant copies, locks, atomics, clock reads, allocations,
and syscalls. Not io_uring specific: anything on the relay's hot path qualifies,
including the shared moq-net model layer and kio.

Every implementation quest lands with a measured before/after (`just bench BASE` on Linux,
plus the targeted micro-benches it names). A measured no-win is a valid
outcome that abandons the quest.

## Plan

Quests branch from main unless they say otherwise.

Planning quests can settle their contracts independently. Facts from the 2026-09
hot-path survey, so quests don't re-litigate them:

- Decided in the 2026-10-05 audit: perf quests that edit moq-uring's QUIC
  driver (Run to quiescence) measure their before and after on `moq-quic`.
- `moq-uring`'s only backend is `moq-quic`. Every profile names its backend. The
  historical quiche-flavor numbers cited in
  [Run to quiescence](/quest/m2/perf/uring-quiescence.md) are re-measured on `moq-quic`.
- Cross-thread wakeups are already cheap: one futex word per worker, at most
  one `futex(FUTEX_WAKE)` per park cycle, wake bursts coalesce through the
  `kio::Tasks` bitset. No eventfd, no MSG_RING, by design (`SINGLE_ISSUER`).
- The io_uring workers are already `!Send` executors (`Rc`/`RefCell`
  throughout `moq-uring`), and moq-net's lite path deliberately carries no
  `Send` bounds. The remaining cross-thread costs live in the shared model:
  one `origin::Producer` spans all workers, so a subscriber on worker B reads
  `kio::Lock` state written on worker A. These quests shrink that cost
  in place; they do not attempt per-worker model sharding, unless the
  [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md)
  spikes measure a win (decided 2026-10-14).
- The batched write/read machinery (`frame::Buffer`, `write_frames`,
  the egress `Prefetch`) already exists in moq-net; egress is amortized,
  ingest and the stream-send path are not.

The relay's `/metrics` endpoint already carries the ring-level counters
(enters, park/wake, batch effectiveness) several quests want as evidence, one
row per io_uring worker.

Decided in the 2026-09-30 audit: io_uring is off by default and ships in no
package, and none of the uring micro-opts is measured on `moq-quic`. The unmeasured
ones moved out (3200 to m2; 3129 with the open contract, 3201, 3202, and 3204
to m3), and NAPI busy polling (3203), registered wait arguments (3205), and
the priority `set_track` wakes were dropped. Egress requeue folded into Run to
quiescence; egress keep-alive folded into Group cost. The benchmark noise
estimate comes first, since every quest here accepts "within noise". The
profiling recipe and One enter per turn rank next: they produce the numbers
that decide the rest.

Decided 2026-10-08: the CPU and allocation profile merged into the profiling
recipe (`just bench-profile`), so the relay has one recipe with capture modes; #3199 moved to m2 beside 3200, since it is an
equally unmeasured ring micro-opt; cache shard was deleted, since #5031's
profile shows no pool contention.

#3122's per-turn clock read landed in
[#5926](https://github.com/moq-dev/moq/pull/5926): the io_uring worker is a
`moq-clock` backend that samples the clock once per turn, cutting
`clock_gettime` from 44.7 to 1.05 calls per turn. `perf` was unavailable
there, so [Profile the io_uring clock read](/quest/m2/perf/uring-clock-profile.md)
owns the `[vdso]` share and relay CPU measurement.

Decided 2026-10-14: kio channel contention ranks first. Its 215% lock wait on
the default tokio runtime is the largest measured cost, and the lock-wait
recipe it needs has shipped. It stays in m2, since m1 defers general
optimization.

Decided 2026-10-22: the WebSocket and TCP stream transports join this line
as the [Stream transports](/quest/m2/perf/stream/README.md) epic, ranked
second, and the io_uring TCP line moves in from m3. io_uring pays far more
for TCP than for UDP, so it no longer waits on fleet demand.

Decided 2026-10-22 from the io_uring audit: `just bench BASE` and
`just bench-profile` run only tokio, so an io_uring quest measures with
`just bench-runtime` at both revisions on the same host and worker count,
and profiles through [Profile the io_uring workers](/quest/m2/perf/uring-profile.md).

## Required

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - workers spend less time blocked on kio channel-state mutexes (up to 215% of worker CPU on chat at 16 tokio workers), via a version counter, per-thread spikes, and an append-only log
- [Stream transports](/quest/m2/perf/stream/README.md) - qmux drives its own WebSocket and TCP sockets in one task, measured end to end, then serves them from the io_uring workers
- [Relay allocations](/quest/m2/perf/relay-allocs.md) - fewer allocations per delivered frame, starting with per-stream session clones
- [Profile the io_uring workers](/quest/m2/perf/uring-profile.md) - `just bench-profile` profiles `io-uring-workers` too, so ring quests stop working around a tokio-only recipe
- [Run to quiescence](/quest/m2/perf/uring-quiescence.md) - a received packet's reply is staged in the same turn, under a pass and train budget that keeps the fairness rule
- [Ingest batch](/quest/m2/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per burst of whole frames instead of per frame
- [Shard by default](/quest/m2/perf/shard-default.md) - a saturating shard sweep decides whether an unset `runtime.workers` runs one worker per core on Linux
- [Profile the io_uring clock read](/quest/m2/perf/uring-clock-profile.md) - a `perf` profile shows whether #5926's per-turn clock read cut the io_uring relay's `[vdso]` share and CPU
- [io_uring buffer pool memory](/quest/m2/perf/uring-pool-memory.md) - measure reserved versus resident pool memory before any shrink policy
