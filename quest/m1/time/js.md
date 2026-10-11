# [M] @moq/time

## Goal

A published `js/time` package, `@moq/time`, mirrors `moq-time`: an `Instant`
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
The installed clock is per JS realm (a page, a worker, a test file), the JS
equivalent of one clock per runtime. Match the Rust timer semantics settled in
[the crate](/quest/m1/time/crate.md), and agree with rs2ts on how a generated
`moq_time::Instant` maps here ([rs2ts](/quest/m1/rs2ts/translator.md)).

Ban direct `performance.now`, `Date.now`, `setTimeout`, and `setInterval` in
the package outside its default backend.

Public API: a new package. Wire: none.

## Required

- [The moq-time crate](/quest/m1/time/crate.md) - the semantics this mirrors
