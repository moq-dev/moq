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
  [Run to quiescence](/quest/m2/perf/uring-quiescence.md) and
  [moq-uring on moq-clock](/quest/m1/time/uring.md) are re-measured on `moq-quic`.
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
recipe ([Relay profiling](/quest/m2/perf/lock-profile.md)), so the relay has
one recipe with capture modes; #3199 moved to m2 beside 3200, since it is an
equally unmeasured ring micro-opt; cache shard was deleted, since #5031's
profile shows no pool contention.

Decided 2026-10-10: #3122's per-turn clock read merged into
[moq-uring on moq-clock](/quest/m1/time/uring.md), where the worker's turn
sample becomes its `moq-clock` clock; it keeps #3122's measurement bar.

Decided 2026-10-14: kio channel contention ranks first. Its 215% lock wait on
the default tokio runtime is the largest measured cost, and the lock-wait
recipe it needs has shipped. It stays in m2, since m1 defers general
optimization.

## Required

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - workers spend less time blocked on kio channel-state mutexes (up to 215% of worker CPU on chat at 16 tokio workers), via a version counter, per-thread spikes, and an append-only log
- [Relay profiling](/quest/m2/perf/lock-profile.md) - one recipe captures lock wait by stack (first), CPU stacks, and heap profiles, with no code in kio
- [One enter per turn](/quest/m2/perf/uring-one-enter.md) - a parking turn pays one io_uring_enter, submits flush deferred completions, and SQEs per enter is a counter
- [Run to quiescence](/quest/m2/perf/uring-quiescence.md) - a received packet's reply is staged in the same turn, under a pass and train budget that keeps the fairness rule
- [Ingest batch](/quest/m2/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per burst of whole frames instead of per frame
- [Remove moq-uring copies](/quest/m2/perf/uring-copies.md) - egress, stream send and receive, and datagram receive stop copying where the QUIC core already allows it, with before/after measurements on `moq-quic`
- [Shard by default](/quest/m2/perf/shard-default.md) - a saturating shard sweep decides whether an unset `runtime.workers` runs one worker per core on Linux

## Related

- [moq-uring on moq-clock](/quest/m1/time/uring.md) - one clock read per worker turn, formerly #3122
