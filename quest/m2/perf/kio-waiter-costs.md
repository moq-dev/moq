# [S] kio waiters stop churning on long-lived drivers and per-chunk watches

## Goal

Three kio waiter costs are measured and fixed where they are real, each with before/after instruction counts:

- A long-lived waiter parked on more than `PARKED` (8) lists is no longer discarded and rebuilt on every poll, measured on a relay session driver.
- IETF fetch serving keeps one close watch per fetch stream instead of building one per chunk.
- A short-lived `kio::log` costs measurably less than about 1.3k instructions to set up and clean up, or the measurement shows why it can't.

## Plan

From [moq-dev/moq#7556](https://github.com/moq-dev/moq/pull/7556) (a measured no-win on adapter waker allocation) and [moq-dev/moq#7531](https://github.com/moq-dev/moq/pull/7531) (borrow-free QUIC state prototypes).

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, in the perf epic, one quest for the three, since they share kio's waiter and log internals and one measurement setup.

- **`Park::hold`'s retire rule** (`rs/kio/src/waiter.rs`): `hold` replaces a waiter that is not `reusable()`, and a waiter parked on more than `PARKED = 8` lists is not. #7556 traced about 4 `Arc<Waker>` per group on a session driver (`Handle::run` / `time::run`) to this: the new waiter's first registration lands on the session-close `Bridge` (moq-uring `adapter.rs` and moq-tokio `owned.rs` `poll_closed`). A relay session driver parks on many more lists, so measure there first (allocations and instructions per delivered group over `just bench-runtime` or a session bench). Then choose between raising `PARKED`, changing the retire rule, or leaving it, on the numbers.
- **IETF fetch close watch** (`rs/moq-net/src/ietf/publisher.rs`): the fetch chunk loop pins a fresh `stream.closed()` per chunk (about lines 2387, 2450, and 2488), so each chunk builds a `kio::wait` whose waiter registers on the send stream's close `Bridge`. Hoist it to one per fetch stream. #7556 did not measure this path; a FETCH delivery bench with many chunks per stream shows the before/after.
- **`kio::log` lifetime cost**: #7531 attributed about 1.3k instructions per short-lived log to its epoch pin, flush, and collect, plus segment retirement on drop. That matters to any design holding one log per stream or per group. Profile a create/append/drop microbench, and cut what doesn't need to run for a log that never grew past its first segment.

A measured no-win for any of the three is a valid outcome; record it here and drop that part.

Public API: none expected. Wire: none.

## Related

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - the append-only log design that would pay `kio::log`'s cost per stream
