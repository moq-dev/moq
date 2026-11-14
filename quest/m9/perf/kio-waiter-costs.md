# [S] kio waiters stop churning on long-lived drivers

## Goal

Two kio waiter costs are measured and fixed where they are real, each with before/after instruction counts:

- A long-lived waiter parked on more than `PARKED` (16) lists is no longer discarded and rebuilt on every poll, measured on a relay session driver.
- A short-lived `kio::log` costs measurably less than about 1.3k instructions to set up and clean up, or the measurement shows why it can't.

## Plan

From [moq-dev/moq#7556](https://github.com/moq-dev/moq/pull/7556) (a measured no-win on adapter waker allocation) and [moq-dev/moq#7531](https://github.com/moq-dev/moq/pull/7531) (borrow-free QUIC state prototypes).

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): in the perf epic, one quest for these, since they share kio's waiter and log internals and one measurement setup.

Moved to lab 2026-11-12 (maintainer, /quest-audit): internal costs with no public API or wire change.

- **`Park::hold`'s retire rule** (`rs/kio/src/waiter.rs`): `hold` replaces a waiter that is not `reusable()`, and a waiter parked on more than `PARKED = 16` lists is not ([moq-dev/moq#7789](https://github.com/moq-dev/moq/pull/7789) raised it from 8 to fix subscription ordering bugs). #7556 traced about 4 `Arc<Waker>` per group on a session driver to this, with the new waiter's first registration landing on the session-close `Bridge` of the async-to-poll adapters. [moq-dev/moq#7771](https://github.com/moq-dev/moq/pull/7771) has since removed those adapters, so re-trace on the native poll transports first. A relay session driver parks on many more lists, so measure there (allocations and instructions per delivered group over `just bench-runtime` or a session bench). Then choose between raising `PARKED`, changing the retire rule, or leaving it, on the numbers.
- **`kio::log` lifetime cost**: #7531 attributed about 1.3k instructions per short-lived log to its epoch pin, flush, and collect, plus segment retirement on drop. That matters to any design holding one log per stream or per group. Profile a create/append/drop microbench, and cut what doesn't need to run for a log that never grew past its first segment.

Dropped 2026-11-12 (/quest-audit): the IETF fetch close-watch part. The fetch path became a poll machine in [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284) (`FetchPayloadWrite` in `rs/moq-net/src/ietf/publisher.rs`), so no chunk builds its own `kio::wait` any more.

A measured no-win for either part is a valid outcome; record it here and drop that part.

Public API: none expected. Wire: none.

## Related

- [kio channel contention](/quest/m9/kio-channel-contention/README.md) - the append-only log design that would pay `kio::log`'s cost per stream
