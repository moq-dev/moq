# [L] One receive budget per worker, shared with the cache

## Goal

Each worker has one byte budget that covers both the receive credit every
connection holds and the groups its cache keeps. A frame's bytes are billed once,
from the first leased byte until the cache drops them. An operator sets that one
number instead of sizing `quic.receive_window` against the cache capacity and the
expected peer count.

Today each connection's window bounds only its own incomplete messages, and the
cache `Pool` is billed again for the same bytes once a frame is taken. So the worst
case is connections × 128 MiB plus the cache. A relay with many peers either
reserves memory it never uses or sizes the window below what one peer's largest
unit needs.

Non-goals: revoking credit already granted, cross-peer eviction, and zero-copy
send buffers (see [registered TX buffers](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md)).

## Plan

Decided 2026-10-21 in the [moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)
iteration:

- **Per worker.** One budget per io_uring worker thread, with no cross-thread
  atomics; the tokio runtime gets one budget. The cache `Pool` splits the same way,
  so each worker's cache and receive credit draw on one number. This reverses the
  relay's single process-wide cache pool, which this quest must migrate (config,
  `MIGRATE.md`, `doc/bin/relay/`).
- **Under pressure, stop granting.** Credit already advertised is never revoked.
  New `MAX_DATA` comes from the budget by fair share, so a peer over its share
  stalls rather than evicting anyone else. Eviction stays within one connection, as
  in #5878. A peer still cannot cost another peer its streams.
- **Accounting first.** Received bytes stay `Bytes` slices, compacted when a reader
  waits on them (#5878). The budget counts leased plus cached bytes once. Move to
  QUICHE-style fixed blocks (8 KiB, allocated lazily and freed as consumed) only if a
  benchmark shows allocator or fragmentation cost the accounting cannot see.

Open, for the implementer to bring to the maintainer:

- **The 64 MiB floor.** Each connection must be able to assemble its largest unit, a
  legacy `AnnounceInit` (`MIN_RECEIVE_WINDOW`). Should a connection get its floor
  only on a `want` that needs it, or should the announcement cap shrink first?
- **The handoff from lease to cache.** A frame taken from a lease keeps its credit
  until complete. Once complete, the credit becomes a cache charge. The charge has
  to move to the cache without a window where the bytes are billed twice or not at
  all.

Lessons from other stacks: QUICHE frees 8 KiB blocks as they are consumed, msquic
never advertises credit it could not back, and quinn and lsquic both let a slice
pin its packet buffer. #5878 already handles the last with compaction.

Benchmark: sweep peers × window over a fixed budget on the `benchmark_receive_windows`
harness. Report peak resident bytes against the budget, evictions, and throughput.

## Related

- [Unbounded groups](/quest/m2/unbounded-groups/README.md) - drops the frame cap that
  sets half of `MIN_RECEIVE_WINDOW`
- [Perf](/quest/m2/perf/README.md) - the per-worker hot paths this budget must not add
  contention to
- [Broadcast fairness](/quest/m2/broadcast-fairness.md) - the receive rank's
  fairness domains
