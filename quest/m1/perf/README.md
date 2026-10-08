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
  driver (#3122, Run to quiescence) Require the
  [hard fork](/quest/m1/quic/fork/README.md), so their before and after are
  measured on `moq-quic` instead of being invalidated by the switch.
- `moq-uring`'s only backend is noq. Every profile names its backend. The
  historical quiche-flavor numbers cited in
  [Run to quiescence](/quest/m1/perf/uring-quiescence.md) and
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

Decided in the 2026-09-30 audit: io_uring is off by default and ships in no
package, and none of the uring micro-opts is measured on noq. The unmeasured
ones moved out (3129, 3200, 3202, the open contract, and cache shard to m2;
3201 and 3204 to m3), and NAPI busy polling (3203), registered wait
arguments (3205), and the priority `set_track` wakes were dropped. Egress
requeue folded into Run to quiescence; egress keep-alive and owned decode
copies folded into Group cost. The benchmark noise estimate and the noq
re-profile come first, since every quest here accepts "within noise". Lock
wait and One enter per turn rank next: they produce the numbers that decide
the rest.

## Required

- [Performance comparisons](/quest/m1/performance-comparisons.md) - the noise estimate every "within noise" verdict here depends on
- [Performance profiles](/quest/m1/performance-profiles.md) - the reproducible noq profile the quests below re-measure on
- [Lock wait](/quest/m1/perf/lock-wait.md) - each worker reports time blocked on cross-worker locks, deciding whether the shared model needs work
- [One enter per turn](/quest/m1/perf/uring-one-enter.md) - a parking turn pays one io_uring_enter, submits flush deferred completions, and SQEs per enter is a counter
- [Group cost](/quest/m1/perf/group-cost.md) - count and cut the allocations and time spent relaying one small group to one viewer
- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - a received packet's reply is staged in the same turn, under a pass and train budget that keeps the fairness rule
- [Announce replay](/quest/m1/perf/announce-replay.md) - the initial announce set replays in linear time, so joins don't slow with the route count
- [Demand aggregate](/quest/m1/perf/demand-aggregate.md) - a track subscribe, leave, or preference update no longer walks every reader of the track
- [Ingest batch](/quest/m1/perf/ingest-batch.md) - relay ingest pays one lock, wake, and clock read per chunk burst instead of per chunk
- [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md) - moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()
- [#3199](/quest/m1/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md) - moq-uring: remove SQ indirection and per-enter ring fd lookup
- [Remove moq-uring copies](/quest/m1/perf/uring-copies.md) - egress, stream send and receive, and datagram receive stop copying where the QUIC core already allows it, after the fork
- [Front rescan](/quest/m1/perf/front-rescan.md) - a route swap handles every ready track query per pass, linear in the front's tracks
