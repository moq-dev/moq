# [M] kio channel contention

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
closure check, the closed check, and the waiter registration. The cache pool
line ([Cache shard](/quest/m2/cache-shard.md)) is not among the hot sites.
`Shared::lock` (`rs/kio/src/shared.rs:47`) shows up too, but stays flat with
the worker count.

Decided (maintainer, 2026-10-07):

- Decide the fix from the stacks. First run the
  [lock profile](/quest/m1/perf/lock-profile.md) to name the contended
  channels (track, group, broadcast, origin state) from the callers above
  these sites. Then bring the options back with numbers. Candidates: a
  lock-free read path (a version counter lets poll and read skip the mutex
  when nothing changed); shrinking the closures that run under the lock;
  restructuring the hottest channel. Submitting staged io_uring SQEs before a
  blocking acquire only hides the stall, so on its own it is not a fix.
- Every change lands with the profile's before and after plus `just bench`;
  a measured no-win is a valid outcome.

## Required

- [Lock profile](/quest/m1/perf/lock-profile.md) - the bpftrace recipe that names the contended channels and measures before and after

## Related

- [Cache shard](/quest/m2/cache-shard.md) - stays parked: the pool line is not where the wait is
