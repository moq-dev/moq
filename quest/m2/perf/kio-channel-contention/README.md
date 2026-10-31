# kio channel contention

## Goal

Relay workers spend less time blocked on kio channel-state mutexes. Measure
it as off-CPU wait share at 1, 4, and 16 workers on the fanout and chat
shapes, before and after.

## Plan

Measured by #5031 (closed 2026-10-07; it completed the measurement half of
the old Lock wait quest). Share is steady-window lock wait over worker
utime+stime:

| runtime | shape | 1 worker | 4 workers | 16 workers |
| --- | --- | --- | --- | --- |
| io_uring | fanout | 0% | 7.8% | setup failed (RLIMIT_MEMLOCK) |
| io_uring | chat | 0% | 3.1% | setup failed (RLIMIT_MEMLOCK) |
| tokio QUIC | fanout | 0% | 5.2% | 36% |
| tokio QUIC | chat | 0.35% | 5.3% | 215% (78s waiting in a 30s window) |

The hot sites are generic channel accessors, so they don't say which channels
are contended. On chat with 16 tokio workers, as process-lifetime totals
across every thread (ramp included, unlike the table's steady window):

- `Consumer::poll` (`rs/kio/src/consumer.rs:79`): 61.4s
- `ConsumerWeak::poll` (`rs/kio/src/weak.rs:276`): 45.5s
- `Consumer::read` (`rs/kio/src/consumer.rs:131`): 11.7s, 1,079,139 contended acquires

`kio::Lock` is a plain `std::sync::Mutex`, and every poll takes it: the
closure check, the closed check, and the waiter registration. The cache
pool's shared counters are not among the hot sites.
`Shared::lock` (`rs/kio/src/shared.rs:47`) shows up too, but stays flat with
the worker count.

Baseline (2026-10-14), the before numbers the fix quests compare against.
Measured on a loaded 24-core Core Ultra 7 270K Plus shared with other jobs
(load average 25 to 42), so these numbers only rank the targets. Each fix
quest reruns its baseline on a quiet host and takes its after on that same
host.

`cargo bench -p kio --bench contended` (`rs/kio/benches/contended.rs`), one
producer and 4,096 consumers on one channel, ns per consumer:

| workload | 1 thread | 4 threads | 16 threads |
| --- | --- | --- | --- |
| notify cycle (`Producer<u64>`) | 37 | 227 | 438 |
| unchanged re-poll | 8.4 | 61 | 76 |
| append log (`GroupState`-shaped) | 44 | 276 | 527 |

Spreading the same consumers over more cores makes the whole cycle slower,
not faster: each acquire that changes cores costs 50 to 500 ns.

`just bench-profile lock`, mutex wait over worker CPU in the 5 s steady window, tokio
workers (`chat` is `bench/workloads/chat.toml`, pinned at 10 messages/s; its
frame size, then rolled once per run from 200 to 400 bytes, is now pinned at 300):

| shape | 1 worker | 4 workers | 16 workers |
| --- | --- | --- | --- |
| fanout | 0% | 1.2% (4 ms) | 9.1% (47 ms) |
| chat | 0% | 5.6% (487 ms) | 30% (3,061 ms) |

Chat saturates its one worker (1.0 core). Neither row is comparable with
#5031's table, which used a different ramp and window.

Contended channels at 16 workers, by share of mutex wait (`moq_net::model`):

| channel | fanout | chat | callers |
| --- | --- | --- | --- |
| `track::TrackState` | 77% | 75% | `GroupExpiry::is_expired` read (32% of all wait in both), `Subscriber::poll_recv_group`, `poll_finished`, `poll_recv_datagram` |
| `resume::Route` | 21% | 23% | `resume::Reader::sync` and `resume::Consumer::end` reads, `poll_changed` |
| `group::GroupState` | 2.3% | 1.6% | frame reads, `drift_edge` |
| `subscription::Subscription` | 0% | 0.6% | `GroupExpiry::is_expired` |

Origin and broadcast state never appear. The `GroupExpiry::is_expired` read
no longer locks the track (or the subscription) while nothing landed above the
group it judges, the common verdict at the live edge
([moq-dev/moq#7327](https://github.com/moq-dev/moq/pull/7327)). Its
`just bench-profile lock` before and after was refused twice on a loaded host
(perf dropped events), so this README's end-to-end rerun is its measurement.
Every hot site is a read or re-poll
of a per-track channel that all of a track's subscribers share, which is the
[version counter](/quest/m2/perf/kio-channel-contention/version.md)'s target.
`GroupState` is not contended here because both shapes send one-frame
groups; `group_contended` in `rs/moq-net/benches/group.rs` measures a
multi-frame group instead.

Decided (maintainer, 2026-10-07):

- Decide the fix from the stacks. First run the
  profiling recipe's lock-wait mode (`just bench-profile lock`) to
  name the contended channels (track, group, broadcast, origin state) from
  the callers above these sites. Then bring the options back with numbers.
  Submitting staged io_uring SQEs before a blocking acquire only hides the
  stall, so on its own it is not a fix.
- Every change lands with the profile's before and after plus `just bench`;
  a measured no-win is a valid outcome.

Decided (maintainer, 2026-10-14):

- This became an epic: a contended micro-bench names the targets, then two
  independent fixes each land their own before and after. This README keeps
  the end-to-end `just bench-profile lock` before and after once the children merge.
- The lock-wait mode `just bench-profile lock` already shipped, so the epic no
  longer waits on the rest of the profiling recipe (CPU and heap modes).
- Scope is `Producer`/`Consumer` only. `Shared` and `Queue` mutate from both
  sides, so a reader-optimized design buys them little; they stay on the
  mutex.
- Breaking kio's API is allowed (e.g. a read becomes a snapshot or a cursor
  rather than a lock guard); moq-net updates in the same PR.
- Candidates kept: a version counter with split waiter lists
  ([Version counter](/quest/m2/perf/kio-channel-contention/version.md)) and
  an append-only log primitive with front eviction
  (Append-only log (`kio::log`)), which
  much of moq-net could adopt
  ([Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md)).
- Rejected, so later sessions don't re-propose them:
  - Left-right (two copies, writer swaps the active one): before reusing the
    old copy the writer must see every reader leave it, an O(consumers) scan
    per publish that grows with fan-out. Every write applies twice, so
    `DerefMut` becomes a replayable op log, and memory per group doubles
    (`Producer::HEAP`). The writer also stalls behind a slow reader.
  - arc-swap / RCU: a write clones `T`, which is O(frames) for `GroupState`.
    It only fits small, rarely written channels, which are not the hot sites.
  - `RwLock`: the critical sections are a few loads, so the reader count's
    cache line bounces between cores just like the mutex.

Decided (maintainer, 2026-10-14, baseline follow-up):

- `GroupExpiry::is_expired` gets its own quest, ranked ahead of the version
  counter, rather than waiting on it.
- The append-only log measures a multi-frame (video-heavy) shape first,
  since both shapes above send one-frame groups.
- The 10 messages/s pin lives only in `bench/workloads/chat.toml`;
  `rs/moq-bench/config/chat.toml` keeps its random rate and frame size.

Decided (maintainer, 2026-10-14, per-thread follow-up):

- Per-worker sharding is no longer ruled out; two spikes decide. Both run on
  the thread-per-core workers (`--runtime-workers`, the default once
  [Shard by default](/quest/m2/perf/shard-default.md) lands) and link workers over an
  in-memory transport (`moq_net::transport::memory`),
  never real QUIC. The
  [replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md)
  replicates the data plane and shares announcements; the
  [cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md)
  shares nothing. A measured win reopens sharding through `/quest-plan`; a
  no-win restores the rule.
- Spikes are draft PRs that never merge; their numbers and recommendation
  land here. The in-memory transport merges for real.
- Flow control throttles only the hop between workers, never the publisher:
  a slow worker lags and skips stale groups like any slow subscriber.
- Log adoption waits on the spikes, since per-thread sessions would leave
  each replica's mutex uncontended. The append-only log itself stopped
  waiting on 2026-10-24 (below).

Group storage benchmark (2026-10-24,
[moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)): four
prototypes of `GroupState`'s frame storage, behind its append, cursor read
(eight frames per call, like `Prefetch`), park, front eviction, and refill
only. The bench is
[`rs/moq-net/benches/storage.rs` at `2cfc0a6`](https://github.com/moq-dev/moq/blob/2cfc0a666dd79d4dbf068ffb27b8059ecb3019e7/rs/moq-net/benches/storage.rs)
and did not merge.

- `deque`: today's `VecDeque` plus offset in the kio mutex.
- `runs`: Front eviction's sorted runs, the tail run inline, in the mutex.
- `log`: write-once slots in refcounted segments (4 doubling to 64 slots),
  a Release-stored length, lock-free cursor reads, an atomic base, and
  refills in a side run below the base behind its own mutex. Parking takes
  a waiter mutex.
- `ring`: a fixed ring of reused slots, each behind an `RwLock` (tokio's
  broadcast design), since a reused slot can't be read lock-free without
  epoch reclamation.

Host: the same 24-core Core Ultra 7 270K Plus, never quiet (load average 29
to 43, 6 to 110 runnable threads at a case's start, median 18). Each case
runs every shape back to back, three passes; the table is the median pass's
10th-percentile round, ns per reader, at 1,024 readers. Rows with 16
readers are dominated by releasing the worker threads and are omitted.

| workload | threads | deque | runs | log | ring |
| --- | --- | --- | --- | --- | --- |
| one-frame groups | 1 | 80 | 80 | 41 | 34 |
| one-frame groups | 4 | 252 | 240 | 151 | 120 |
| one-frame groups | 16 | 283 | 275 | 175 | 140 |
| edge, 1 frame per round | 1 | 58 | 58 | 51 | 59 |
| edge, 1 frame per round | 4 | 222 | 219 | 197 | 217 |
| edge, 1 frame per round | 16 | 497 to 1,412 | 477 to 2,581 | 344 to 2,901 | 347 to 432 |
| burst, 8 frames per round | 1 | 132 | 132 | 125 | 185 |
| burst, 8 frames per round | 4 | 409 | 391 | 256 | 501 |
| burst, 8 frames per round | 16 | 682 | 630 | 416 | 474 |
| refill beside the tail | 1 | - | 58 | 65 | 68 |
| refill beside the tail | 4 | - | 217 | 198 | 222 |
| refill beside the tail | 16 | - | 472 | 358 | 358 |

The 16-thread edge row varied that much between passes, so it ranks
nothing. One thread appending a 60-frame group, ns per frame (Criterion
median): deque 30, runs 30, log 46, ring 42. Draining it: 14, 15, 12, 19.

- `runs` costs what `deque` does everywhere, so the shape is free. It is not
  faster: refills and the tail share one mutex.
- `log` costs 10 to 40% less per reader across cores, most when a reader
  drains several frames, and within 15% either way on one thread. Its
  writer pays about 15 ns more per append (the SeqCst fence before checking for parked
  readers, and the slot's `OnceLock`). Parking still takes a mutex, so an
  edge reader saves one of its two acquires per wake, not both.
- `ring` is rejected. A group with no frame cap has no size to give it, and
  growing stops every reader or needs epochs. Its per-slot locks cost 40%
  more for a burst on one thread and lose to the log across cores. It wins
  one-frame groups only because its reader holds no channel handle.
- Not run: `just bench-profile lock` on a multi-frame shape
  (`bench/workloads/video-heavy.toml` is one), which needs
  `kernel.perf_event_paranoid=-1` and this host has 4. So a relay's share of
  `GroupState` wait with multi-frame groups is still unmeasured.

Decided (maintainer, 2026-10-24): Front eviction lands on the lock-free
append-only log (`kio::log`) itself, a tail run from
an advancing base with refilled runs below it, rather than the same layout
under today's mutex with the log swapped in later. The maintainer preferred
landing the end-state storage once. So the log moved to m1, ahead of Front
eviction, scoped to `GroupState`'s frames with the above as its expected
win, and no longer waits on the spikes: they change how much it wins, not
the storage Front eviction needs. `TrackState` arrival order moved to
[Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md), which
still waits on them.
A 2026-10-22 staging CDN chat profile (default tokio runtime, `f57c0aad`,
DWARF stacks, 36% incomplete) put waiter registration at 1.97% self-time, the
fourth-largest site, on a 501-connection chat shape. It agrees with this
epic's targets but carries no lock-wait split.

## Required

- [Version counter](/quest/m2/perf/kio-channel-contention/version.md) - an opted-in re-poll skips the mutex when nothing changed, and parking no longer contends with readers
- [Replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md) - measures per-worker data-plane replicas linked over the in-memory transport, announcements shared
- [Cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md) - measures a relay per thread clustered over the in-memory transport, nothing shared
- [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md) - the rest of moq-net's log-shaped state moves onto the log where it measures as a win
