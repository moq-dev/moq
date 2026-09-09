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

This line starts after the dev merge, on main. Facts from the 2026-09
hot-path survey, so quests don't re-litigate them:

- The default `moq-uring` backend is noq, compiled through the `quinn/`
  module: `quic/mod.rs` selects `quinn/mod.rs` for the `noq` feature
  (rs/moq-uring/src/quic/mod.rs:49-51) and that module aliases `noq_proto as
  quinn_proto` (quinn/mod.rs:26). The relay's `io-uring` feature is that
  backend; `io-uring-quinn` and `io-uring-quiche` are the explicit
  alternatives (rs/moq-relay/Cargo.toml:55-57). Every profile names its
  backend. The quiche-only citations in
  [Egress requeue](/quest/m2/perf/egress-requeue.md) and
  [#3122](/quest/m2/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md)
  describe the non-default path.
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
row per io_uring worker. The
[noq parity gate](/quest/m2/quic/noq-parity.md) benchmarks noq against the
quiche backend; the zero-copy quests here stay independently measured on the
default backend.

## Quests

- [Ingest batch](/quest/m2/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per chunk burst instead of per chunk
- [#3122](/quest/m2/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [Cache shard](/quest/m2/perf/cache-shard.md) - stop hammering one process-global cache line from every worker
- [#3199](/quest/m2/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
- [#3200](/quest/m2/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - moq-uring: batch completion wakeups with MIN_TIMEOUT
- [#3129](/quest/m2/perf/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - moq-uring: write the WebTransport stream header at open time, so finish() never owes one
- [Egress requeue](/quest/m2/perf/egress-requeue.md) - trains per turn on the egress driver becomes a measured budget instead of a hardcoded one
- [#3201](/quest/m2/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - moq-uring: use SENDMSG_ZC for large UDP GSO trains
- [#3202](/quest/m2/perf/3202-moq-uring-use-fixed-file-slots-for-worker-udp-sockets.md) - moq-uring: use fixed-file slots for worker UDP sockets
- [#3204](/quest/m2/perf/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) - moq-uring: register TX-pool buffers for zero-copy sends
- [Send order width](/quest/m2/perf/send-order-width.md) - a wider transport send order lets a group rank itself instead of taking the queue lock
- [Priority set_track wakes](/quest/m2/perf/priority-set-track-wakes.md) - a track priority change stops waking groups that end up where they started
- [#3203](/quest/m2/perf/3203-moq-uring-add-opt-in-napi-busy-polling.md) - moq-uring: add opt-in NAPI busy polling
- [#3205](/quest/m2/perf/3205-moq-uring-register-reusable-io-uring-enter-wait-arguments.md) - moq-uring: register reusable io_uring_enter wait arguments

## Related

- [noq parity gate](/quest/m2/quic/noq-parity.md) - the benchmark that
  decides whether quiche can go, run on these worker primitives
