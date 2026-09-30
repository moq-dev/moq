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

Quests branch from main unless they say otherwise;
[Run to quiescence](/quest/m1/perf/uring-quiescence.md) needs dev, where
`kio`'s `Tasks::poll` changed (#4156).

Planning quests can settle their contracts independently. Facts from the 2026-09
hot-path survey, so quests don't re-litigate them:

- `moq-uring`'s only backend is noq. Every profile names its backend. The
  historical quiche-flavor numbers cited in
  [Egress requeue](/quest/m1/perf/egress-requeue.md) and
  [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md)
  are re-measured on noq.
- Cross-thread wakeups are already cheap: one futex word per worker, at most
  one `futex(FUTEX_WAKE)` per park cycle, wake bursts coalesce through the
  `kio::Tasks` bitset. No eventfd, no MSG_RING, by design (`SINGLE_ISSUER`).
- The io_uring workers are already `!Send` executors (`Rc`/`RefCell`
  throughout `moq-uring`), and moq-net's lite path deliberately carries no
  `Send` bounds. The remaining cross-thread costs live in the shared model:
  one `origin::Producer` spans all workers, so a subscriber on worker B reads
  `kio::Lock` state written on worker A. These quests shrink that cost
  in place; they do not attempt per-worker model sharding.
- The batched write/read machinery (`frame::Buffer`, `write_frames`,
  the egress `Prefetch`) already exists in moq-net; egress is amortized,
  ingest and the stream-send path are not.

The relay's `/metrics` endpoint already carries the ring-level counters
(enters, park/wake, batch effectiveness) several quests want as evidence, one
row per io_uring worker.

## Required

- [Announce replay](/quest/m1/perf/announce-replay.md) - the initial announce set replays in linear time, so joins don't slow with the route count
- [Group cost](/quest/m1/perf/group-cost.md) - count and cut the allocations and time spent relaying one small group to one viewer
- [One enter per turn](/quest/m1/perf/uring-one-enter.md) - a parking turn pays one io_uring_enter, submits flush deferred completions, and SQEs per enter is a counter
- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - a received packet's reply is staged in the same turn, under a pass budget that keeps the fairness rule
- [Lock wait](/quest/m1/perf/lock-wait.md) - each worker reports time blocked on cross-worker locks, deciding whether the shared model needs work
- [Ingest batch](/quest/m1/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per chunk burst instead of per chunk
- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [#3199](/quest/m1/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
