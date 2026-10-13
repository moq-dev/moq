# [M] @moq/time

## Goal

A published `js/time` package, `@moq/time`, mirrors `moq-clock`: an `Instant`
(a `number` of nanoseconds from the clock's origin), the `Milli`, `Micro`,
`Nano`, and `Second` duration units, a wall-clock timestamp, and ambient
`now()`, `wall()`, `sleep()`, `timeout()`, and `interval()` that resolve to
an installed clock (by default `performance.now()`, `Date.now()`, and
`setTimeout`). Tests install a `Manual` clock with scoped restore
(`advance()`, `stepWall()`, and an async helper that runs due timers and
drains microtasks) instead of patching globals, so a host's own timers (the
workerd test runner's RPC timers, for one) stay real. `doc/lib/js` documents
it.

It lands additively on `main`; `@moq/net` keeps its own units until
[JS on @moq/time](/quest/m1/time/js-migrate.md).

## Plan

The design decisions are in [the line's README](/quest/m1/time/README.md).

Open: mirror the Rust name as `@moq/clock`, or keep `@moq/time`. `@moq/clock`
is unpublished on npm but names the private `js/clock` demo (bin
`moq-clock`), which would need renaming first.
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
