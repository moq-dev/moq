# Migrating @moq/signals

How to update your code for each breaking change, newest first.

## Unreleased

### Timers take a `Nano` (#5706)

`effect.timer`, `effect.timeout`, and `effect.interval` take a `Nano` from `@moq/clock` instead of milliseconds, and run on the installed clock.
Convert a millisecond value with `Nano.fromMilli(Milli(250))`, importing both from `@moq/clock`.

### Effect cleanups run last-in, first-out (#4889)

An effect tears down its cleanups newest first, like `DisposableStack`.
A resource registered later, such as a nested `run` built on an earlier one, is now released before what it depends on.
Check any cleanup that relied on running after one registered later.

### `Signal.race` returns a `GetPromise` (#4226)

`Signal.race` releases its losing listeners and returns a `GetPromise` instead of a native `Promise`.
`await` and `.then()` still work; `.catch()` and `.finally()` do not, so wrap the `await` in `try`/`catch` instead.
Waiting begins when it is awaited or subscribed.
