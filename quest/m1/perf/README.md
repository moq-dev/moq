# Perf questline

## Goal

Reduce relay CPU per session, raise the per-worker throughput ceiling, and
hold tail latency on the dev thread-per-core stack by eliminating measured
hot-path costs: redundant copies, locks, atomics, clock reads, allocations,
and syscalls. Not io_uring specific: anything on dev's hot path qualifies,
including the shared moq-net model layer and kio.

Every quest lands with a measured before/after (`just bench BASE` on Linux,
plus the targeted micro-benches it names). A measured no-win is a valid
outcome that abandons the quest.

## Plan

Branch every quest from dev. Facts from the 2026-09 hot-path survey, so
quests don't re-litigate them:

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

[Worker metrics](/quest/m1/uring-metrics.md) is a soft dependency: it adds
the ring-level counters (enters, park/wake, batch effectiveness) several
quests want as evidence. Use bench CPU/RSS until it lands. The
[QUIC backend bakeoff](/quest/m3/quic-backend-bakeoff.md) has its own
`SendMsgZc` measurement axis; the zero-copy quests here stay independently
measured and scoped to the quiche backend we ship today.

## Quests

- [Ingest batch](/quest/m1/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per chunk burst instead of per chunk
- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [Cache shard](/quest/m1/perf/cache-shard.md) - stop hammering one process-global cache line from every worker
- [Slot flags](/quest/m1/perf/slot-flags.md) - track delivery stops taking nested group locks under the track lock
- [#3199](/quest/m1/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
- [#3200](/quest/m1/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3129](/quest/m1/perf/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [#3201](/quest/m1/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - moq-uring: use SENDMSG_ZC for large UDP GSO trains
- [#3202](/quest/m1/perf/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [#3204](/quest/m1/perf/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Session micro](/quest/m1/perf/session-micro.md) - per-datagram route lookups and per-chunk stats bumps get amortized
- [Send order width](/quest/m1/perf/send-order-width.md) - a wider transport send order lets a group rank itself instead of taking the queue lock
- [Priority set_track wakes](/quest/m1/perf/priority-set-track-wakes.md) - a track priority change stops waking groups that end up where they started
- [#3203](/quest/m1/perf/3203-moq-uring-add-opt-in-napi-busy-polling.md) - moq-uring: add opt-in NAPI busy polling
- [#3205](/quest/m1/perf/3205-moq-uring-register-reusable-io-uring-enter-wait-arguments.md) - moq-uring: register reusable io_uring_enter wait arguments

## Related

- [Worker metrics](/quest/m1/uring-metrics.md) - the counters these quests are judged by
- [QUIC backend bakeoff](/quest/m3/quic-backend-bakeoff.md) - overlapping SendMsgZc axis at larger measurement scope
