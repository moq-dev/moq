# [M] kio version counter

## Goal

A `Consumer` re-poll that opts into version tracking and finds nothing
changed returns without taking the channel mutex, and parking a waiter no
longer contends with readers of the value. Lands with the contended bench (`rs/kio/benches/contended.rs`) and
`just bench-profile lock` before and after, against the
[epic](/quest/m2/perf/kio-channel-contention/README.md)'s baseline; a measured no-win abandons it.

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
  append-only log (`kio::log`) covers that.
- The skip is opt-in, never a silent change to `poll`: a predicate may read
  state outside the channel. `Settle::poll` (`rs/moq-net/src/tail.rs`)
  captures a deadline's `expired` in its closure, so skipping it on an
  unchanged version after the deadline's wake would park the task forever.
  Only callers whose predicate depends solely on `T` use the version-aware
  path.
- Open: the shape of that path and where the last-seen version lives (the
  `Consumer`, the `Waiter`, or a caller-held cursor). The `Consumer` is shared
  by `&self` across waiters, which argues for a caller-held cursor. Settle it
  first, with options and a recommendation.
- Generic across every `T`, with no moq-net restructure.

