# Perf questline

## Goal

Reduce relay CPU per session, raise the per-worker throughput ceiling, and
hold tail latency on the dev thread-per-core stack by eliminating measured
hot-path costs: redundant copies, locks, atomics, clock reads, allocations,
and syscalls. Not io_uring specific: anything on dev's hot path qualifies,
including the shared moq-net model layer and kio.

Every implementation quest lands with a measured before/after (`just bench BASE` on Linux,
plus the targeted micro-benches it names). A measured no-win is a valid
outcome that abandons the quest.

## Plan

Implementations start after the dev merge, on main; planning quests can settle
their contracts independently. Facts from the 2026-09
hot-path survey, so quests don't re-litigate them:

- `moq-uring`'s only backend is noq. Every profile names its backend. The
  historical quiche-flavor numbers cited in
  [Egress requeue](/quest/next/perf/egress-requeue.md) and
  [#3122](/quest/next/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md)
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

## Quests

- [Open contract](/quest/next/perf/uring-open-contract.md) - plan concurrent WebTransport opening and cancellation

- [One enter per turn](/quest/next/perf/uring-one-enter.md) - a parking turn pays one io_uring_enter, submits flush deferred completions, and SQEs per enter is a counter
- [Run to quiescence](/quest/next/perf/uring-quiescence.md) - a received packet's reply is staged in the same turn, under a pass budget that keeps the fairness rule
- [Lock wait](/quest/next/perf/lock-wait.md) - each worker reports time blocked on cross-worker locks, deciding whether the shared model needs work
- [Ingest batch](/quest/next/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per chunk burst instead of per chunk
- [Egress cache refresh](/quest/next/perf/egress-keepalive.md) - measure refresh costs while preserving slow-reader retention
- [Owned decoding copies](/quest/next/perf/coding-decode.md) - measure and reduce owned decode allocations and copies
- [#3122](/quest/next/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [Cache shard](/quest/next/perf/cache-shard.md) - stop hammering one process-global cache line from every worker
- [#3199](/quest/next/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
- [#3200](/quest/next/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3129](/quest/next/perf/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [Egress requeue](/quest/next/perf/egress-requeue.md) - trains per turn on the egress driver becomes a measured budget instead of a hardcoded one
- [#3201](/quest/next/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - moq-uring: use SENDMSG_ZC for large UDP GSO trains
- [#3202](/quest/next/perf/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [#3204](/quest/next/perf/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Priority set_track wakes](/quest/next/perf/priority-set-track-wakes.md) - a track priority change stops waking groups that end up where they started
- [#3203](/quest/next/perf/3203-moq-uring-add-opt-in-napi-busy-polling.md) - moq-uring: add opt-in NAPI busy polling
- [#3205](/quest/next/perf/3205-moq-uring-register-reusable-io-uring-enter-wait-arguments.md) - moq-uring: register reusable io_uring_enter wait arguments

## Related

- [Send buffer pools](/quest/future/quic-buffer-pool.md) - the stream-send
  allocation question, measured on the same shapes
- [Origin lookup CPU](/quest/next/origin-cpu/README.md) - announce/subscribe table, not uring
