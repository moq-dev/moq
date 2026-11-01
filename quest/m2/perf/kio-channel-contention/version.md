# [M] kio version counter

## Goal

A `Consumer` re-poll that opts into version tracking and finds nothing
changed returns without taking the channel mutex, and parking a waiter no
longer contends with readers of the value. Lands with the contended bench (`rs/kio/benches/contended.rs`) and
`just bench-profile lock` before and after, against the
[epic](/quest/m2/perf/kio-channel-contention/README.md)'s baseline; a measured no-win abandons it.

## Plan

The core landed in
[moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532), with no caller
opted in yet (`rs/kio/src/channel.rs`):

- An `AtomicU64` version beside the value, bumped under the value lock when a
  `Mut` drops modified (and on close, as its low bit), published with Release
  before the wake.
- The waiter lists sit under their own lock. Registration re-checks the
  version after registering and wakes its own task if a publish slipped in.
  The value list's drain tag is mirrored beside the version, so a waiter still
  parked from an earlier poll re-polls without the waiter lock.
- `is_closed` and the closed checks are atomic loads.

Its cost on the paths nobody opts into, in instructions on one pinned core
(one codegen unit, before to after): unchanged re-poll 73 to 79, notify cycle
263 to 337 per consumer (the re-park takes the waiter lock), a write nobody is
parked on 110 to 159 (the publish and the waiter lock), `is_closed` 32 to 10.

What remains:

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
  by `&self` across waiters, which argues for a caller-held cursor. The options
  and a recommendation are in
  [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532). A prototype
  cursor (`Consumer::poll_since(&self, &Waiter, &mut u64, f)`, the version
  stored only when `f` is pending) skips both locks on an unchanged version and
  a still-parked waiter: the contended unchanged re-poll went from 8.5, 62, and
  74 ns per consumer at 1, 4, and 16 threads to 1.4, 0.4, and 0.2, and from 73
  to 58 instructions.
- Then move moq-net's hot re-polls whose predicates depend only on `T` onto
  it, and take the contended bench and `just bench-profile lock` after.
- Generic across every `T`, with no moq-net restructure.

