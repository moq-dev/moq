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

`just bench-lock`, mutex wait over worker CPU in the 5 s steady window, tokio
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
is its own fix, [Group expiry](/quest/m2/perf/kio-channel-contention/group-expiry.md).
Every hot site is a read or re-poll
of a per-track channel that all of a track's subscribers share, which is the
[version counter](/quest/m2/perf/kio-channel-contention/version.md)'s target.
`GroupState` is not contended here because both shapes send one-frame
groups; a multi-frame shape is unmeasured, so the
[append-only log](/quest/m2/perf/kio-channel-contention/log.md) should measure
one before naming `GroupState` frames as its target.

Decided (maintainer, 2026-10-07):

- Decide the fix from the stacks. First run the
  [profiling recipe](/quest/m2/perf/lock-profile.md)'s lock-wait mode to
  name the contended channels (track, group, broadcast, origin state) from
  the callers above these sites. Then bring the options back with numbers.
  Submitting staged io_uring SQEs before a blocking acquire only hides the
  stall, so on its own it is not a fix.
- Every change lands with the profile's before and after plus `just bench`;
  a measured no-win is a valid outcome.

Decided (maintainer, 2026-10-14):

- This became an epic: a contended micro-bench names the targets, then two
  independent fixes each land their own before and after. This README keeps
  the end-to-end `just bench-lock` before and after once the children merge.
- The lock-wait mode `just bench-lock` already shipped, so the epic no
  longer waits on the rest of the profiling recipe (CPU and heap modes).
- Scope is `Producer`/`Consumer` only. `Shared` and `Queue` mutate from both
  sides, so a reader-optimized design buys them little; they stay on the
  mutex.
- Breaking kio's API is allowed (e.g. a read becomes a snapshot or a cursor
  rather than a lock guard); moq-net updates in the same PR.
- Candidates kept: a version counter with split waiter lists
  ([Version counter](/quest/m2/perf/kio-channel-contention/version.md)) and
  an append-only log primitive with front eviction
  ([Append-only log](/quest/m2/perf/kio-channel-contention/log.md)), which
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
- The append-only log measures a multi-frame (video-heavy) shape before it
  picks targets, since both shapes above send one-frame groups.
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
- The append-only log waits on the spikes, since per-thread sessions would
  leave each replica's mutex uncontended.

## Required

- [Group expiry](/quest/m2/perf/kio-channel-contention/group-expiry.md) - a group poll decides expiry without locking the track state, the largest single wait site
- [Version counter](/quest/m2/perf/kio-channel-contention/version.md) - an opted-in re-poll skips the mutex when nothing changed, and parking no longer contends with readers
- [Replica spike](/quest/m2/perf/kio-channel-contention/spike-replica.md) - measures per-worker data-plane replicas linked over the in-memory transport, announcements shared
- [Cluster spike](/quest/m2/perf/kio-channel-contention/spike-cluster.md) - measures a relay per thread clustered over the in-memory transport, nothing shared
- [Append-only log](/quest/m2/perf/kio-channel-contention/log.md) - a kio log primitive with lock-free cursor reads and front eviction, if the shared model survives the spikes
- [Log adoption](/quest/m2/perf/kio-channel-contention/log-adoption.md) - the rest of moq-net's log-shaped state moves onto the log where it measures as a win

## Related

- [Relay profiling](/quest/m2/perf/lock-profile.md) - the lock-wait recipe that measures before and after
