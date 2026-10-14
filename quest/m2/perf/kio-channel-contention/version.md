# [M] kio version counter

## Goal

A `Consumer` re-poll that finds nothing changed returns without taking the
channel mutex, and parking a waiter no longer contends with readers of the
value. Lands with the [contended bench](/quest/m2/perf/kio-channel-contention/bench.md)
and `just bench-lock` before and after; a measured no-win abandons it.

## Plan

- An `AtomicU64` version beside the state, bumped under the lock when a `Mut`
  drops modified (and on close), published with Release before the wake.
- The waiter lists move out from under the value mutex into their own lock.
  Registration re-checks the version after registering, which is what keeps
  wakeups from being lost now that the value check and the registration no
  longer share one lock.
- `is_closed` and the closed check become atomic loads.
- This attacks spurious polls, not the wake-all herd: a woken consumer whose
  data really changed still locks to read `T`. The
  [append-only log](/quest/m2/perf/kio-channel-contention/log.md) covers that.
- Open: where the last-seen version lives (the `Consumer`, the `Waiter`, or a
  caller-held cursor). The `Consumer` is shared by `&self` across waiters, so
  the answer decides whether this stays API-compatible. Settle it first, with
  options and a recommendation.
- Every new atomic protocol gets a loom model in `rs/kio/src/loom.rs`.
- Generic across every `T`, with no moq-net restructure.

## Required

- [Contended bench](/quest/m2/perf/kio-channel-contention/bench.md) - the before numbers and the bench this is measured on
