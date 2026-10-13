# [M] @moq/clock

## Goal

A published `js/clock` package, `@moq/clock`, mirrors `moq-clock`: an `Instant`
(a `number` of nanoseconds from the clock's origin), the `Milli`, `Micro`,
`Nano`, and `Second` duration units, a `Wall` wall-clock time, and ambient
`now()`, `wall()`, `sleep()`, `timeout()`, and `interval()` that resolve to
an installed clock (by default `performance.now()`, `Date.now()`, and
`setTimeout`). Tests install a `Manual` clock with scoped restore
(`advance()`, `stepWall()`, and an async helper that runs due timers and
drains microtasks) instead of patching globals, so a host's own timers (the
workerd test runner's RPC timers, for one) stay real. `doc/lib/js` documents
it.

It lands additively on `main`; `@moq/net` keeps its own units until
[JS on @moq/clock](/quest/m1/time/js-migrate.md).

## Plan

The design decisions are in [the line's README](/quest/m1/time/README.md).

Named `@moq/clock` to mirror `moq-clock` (decided 2026-10-13 by the
maintainer). The name is unpublished on npm; the private CLOCK demo that held
it in `js/clock` was deleted in #5465. A `Manual` clock starts where Rust's
does: instant zero and 2025-01-01 UTC wall time.

The installed clock is per JS realm (a page, a worker, a test file), the JS
equivalent of one clock per runtime. Match the timer semantics `moq-clock`
settled and tests: an already-reached deadline fires at once, equal deadlines
fire in arming order, and `interval` defaults to `Burst` with `Delay` and
`Skip` available. Coordinate clock origin, units, and
controlled-time behavior with the [browser backend](/quest/m2/rs2ts/README.md);
its boundary representation depends on whether it uses WASM or generated JS.
`moq-clock`'s browser backend counts from its first read, not from
`performance.now()`'s origin, so the two need one origin before an instant
crosses between them.

Ban direct `performance.now`, `Date.now`, `setTimeout`, and `setInterval` in
the package outside its default backend.

Public API: a new package. Wire: none.
