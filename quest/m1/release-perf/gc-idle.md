# [S] Idle GC

## Goal

`track::Pool::advance` with nothing due does no clock read, lock, or waker
registration. `track_gc/idle` returns to a few nanoseconds.

## Plan

Found by reading code on 2026-10-21 (11.6 ns to 74 ns, flat across sizes,
confirmed). [#5548](https://github.com/moq-dev/moq/pull/5548) (wall-clock
aging of superseded groups) added the following to every `advance` in
`rs/moq-net/src/model/cache.rs`:

- the `wall_epoch` `OnceLock` and a `wall_micros.fetch_max`;
- `release_due(self.now_wall(), driver)`, which reads `moq_clock::now()`;
- the `inner.wall` lock and `driver.register`.

Part of the cost is the bench itself: it builds a fresh `kio::Waiter::noop()`
on every iteration, so each `register` allocates and frees an `Arc<Waker>`. A
real driver reuses its waiter. Hoist the waiter out of `b.iter` first, and
measure what remains.

Fix: keep the earliest wall deadline in an atomic, as `expiry.rs` already
does with `earliest`. Read `now_wall()` and take the lock only when that
deadline has passed. Time decisions use `max(wall, pts)`; keep that behavior.


### Progress

The draft implementation in [moq-dev/moq#6291](https://github.com/moq-dev/moq/pull/6291)
hoists the benchmark waiters before measuring either idle collector. It also
measures advancing supplied instants, alongside the existing sweeps over track
counts and wall-deadline track/group counts.

The private collector now skips the ambient clock read when the wall index is
empty. It keeps one lazy wall sample for the whole release pass when the index
is nonempty. The sampled clock's `fetch_max` became a store: its only writer
already holds the clock mutex and clamps supplied time against the last sample.
The wall index still registers every driver under its existing lock, including a
replacement driver arriving while no deadline is armed. No public API, wire,
aging, or ownership changes are proposed.

Reused-waiter baseline on 2026-10-22: `track_gc/idle` is 36.7-36.9 ns over
1/64/1,024/16,384 tracks. Initial private-change measurements are 19.5-19.9 ns;
wall-index idle remains flat in both track and cached-group counts. The added
simulated-time test covers ambient time leading a stale driver, supplied future
time leading the ambient clock, and a backward replacement sample. Another test
covers replacement-driver registration and rearming after a deadline drains.

### Decisions needed

The original no-lock/no-registration goal is not met. Recommend accepting the
remaining flat cost while preserving the current `gc(now, waiter)` contract:

- An empty wall index still needs to register a new driver, so it hears the first
  deadline armed after it parks. An earliest-deadline atomic alone cannot prove
  that this particular waiter remains registered after another driver drops or a
  previous wake drains the list.
- A nonempty wall index still needs the ambient clock even if supplied `now` is
  before its earliest deadline. A lagging driver must release deadlines reached
  by ambient time; comparing only supplied time would change aging semantics.

Alternatively, design driver registration ownership and a reusable lock-free
notification primitive before changing the collector's contract. That public
API decision is intentionally left to the maintainer. The quest remains active
until its stronger goal is revised or implemented.
