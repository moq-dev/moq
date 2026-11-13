# Perf questline

## Goal

Reduce relay CPU per session, raise the per-worker throughput ceiling, and
hold tail latency on the thread-per-core stack by eliminating measured
hot-path costs: redundant copies, locks, atomics, clock reads, allocations,
and syscalls. Not io_uring specific: anything on the relay's hot path qualifies,
including the shared moq-net model layer and kio.

Every implementation quest lands with a measured before/after (`just bench BASE` on Linux,
or `just bench-runtime` for an io_uring change, plus the targeted micro-benches it names). A measured no-win is a valid
outcome that abandons the quest.

## Plan

Quests branch from main unless they say otherwise.

Planning quests can settle their contracts independently. Facts from the 2026-09
hot-path survey, so quests don't re-litigate them:

- Decided in the 2026-10-05 audit: perf quests that edit moq-uring's QUIC
  driver measure their before and after on `moq-quic`.
- `moq-uring`'s only backend is `moq-quic`. Every profile names its backend.
- Cross-thread wakeups are already cheap: one futex word per worker, at most
  one `futex(FUTEX_WAKE)` per park cycle, wake bursts coalesce through the
  `kio::Tasks` bitset. No eventfd, no MSG_RING, by design (`SINGLE_ISSUER`).
- The io_uring workers are already `!Send` executors (`Rc`/`RefCell`
  throughout `moq-uring`), and moq-net's lite path deliberately carries no
  `Send` bounds. The remaining cross-thread costs live in the shared model:
  one `origin::Producer` spans all workers, so a subscriber on worker B reads
  `kio::Lock` state written on worker A. These quests shrink that cost
  in place; they do not attempt per-worker model sharding, unless the
  [kio channel contention](/quest/m9/kio-channel-contention/README.md)
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
quiescence.

Decided 2026-10-08: the CPU and allocation profile merged into the profiling
recipe (`just bench-profile`), so the relay has one recipe with capture modes; #3199 moved to m2 beside 3200, since it is an
equally unmeasured ring micro-opt; cache shard was deleted, since #5031's
profile shows no pool contention.

#3122's per-turn clock read landed in
[#5926](https://github.com/moq-dev/moq/pull/5926): the io_uring worker is a
`kio::clock` backend that samples the clock once per turn, cutting
`clock_gettime` from 44.7 to 1.05 calls per turn. A later `perf` profile
([#7505](https://github.com/moq-dev/moq/pull/7505)) put the io_uring relay's
`[vdso]` cycle share at 0.5% before #5926 and 0.0 to 0.2% after, below the
tokio workers' 0.2 to 0.4%. #5926 also cut chat's relay instructions per
delivered frame by about 5%; video's 1.5% drop was within noise. The remaining
`[vdso]` samples come mostly from the per-turn read and `moq_net`
`Timestamp::from(Instant)`.

Decided 2026-10-14: kio channel contention ranks first. Its 215% lock wait on
the default tokio runtime is the largest measured cost, and the lock-wait
recipe it needs has shipped. It stays in later work (m4), since the release defers general
optimization.

Decided 2026-10-22: the WebSocket and TCP stream transports join this line
as the [Stream transports](/quest/m9/stream-transports/README.md) epic, ranked
second, and the io_uring TCP line moves in from m3. io_uring pays far more
for TCP than for UDP, so it no longer waits on fleet demand.

Decided 2026-10-22 from the io_uring audit: `just bench BASE` runs only
tokio, so an io_uring quest measures with `just bench-runtime` at both
revisions on the same host and worker count, and profiles with
`just bench-profile MODES WORKLOAD WORKERS io-uring-workers [BASE]`.

Moved to lab 2026-11-12 (maintainer, /quest-audit): every child measures
first and touches no published API or wire. The epic no longer lists its two
sibling epics under `Required`, which held all fourteen children behind an
L-sized port; they are part of this line, not blockers of it.

## Related

- [kio channel contention](/quest/m9/kio-channel-contention/README.md) - workers spend less time blocked on kio channel-state mutexes
- [Stream transports](/quest/m9/stream-transports/README.md) - qmux drives its own WebSocket and TCP sockets, then serves them from the io_uring workers
