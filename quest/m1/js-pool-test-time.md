# [S] pool.test.ts on fake timers

## Goal

`js/net/src/connection/pool.test.ts` neither reads the wall clock nor waits on
a real timer. The linger, retry-window, and outage tests advance Bun's fake
timers instead of sleeping or patching `performance.now`, and each still fails
against the bug it was written for.

## Plan

Raised in #5355's review: its own regression moved to fake timers, the rest of
the file did not. Still on real time as of `079d7d8d4`:

- `expired()` waits `linger * 15` real milliseconds, and the longest-linger
  test waits 50 ms.
- `waitUntil` polls `Date.now` through real `setTimeout(0)` settles, for up to
  1 s, and 15 s in the shared-outage test.
- The shared-outage and private-partial-delay tests speed up the retry window
  by monkeypatching `performance.now`, and the shared one sleeps 2 s under a
  30 s test timeout.

Decided 2026-10-13: switch to Bun's fake timers now, as a standalone quest,
rather than waiting for `@moq/time`.

Use Bun's `jest.useFakeTimers()`, which also drives `performance.now` (#5355),
as `connect.test.ts`, `js/publish/src/source/retry.test.ts`, and #5355's test
already do. Check that it drives `Date.now` too, or drop the `Date` reads
(left to the implementer, decided 2026-10-13).
Replace `settle`, `waitUntil`, and `expired` with one helper that advances the
clock and drains microtasks; the mock transport's handshake must still finish
inside a drain.

This doesn't wait for `@moq/time`: [JS on @moq/time](/quest/m1/time/js-migrate.md)
already moves every `jest.useFakeTimers` site onto its `Manual` clock, so the
swap there is mechanical, and these tests stop costing seconds and load
sensitivity now. A test that still seems to need a real wait has a cause to
fix, not a wait to widen.

Public API: none. Wire: none.

## Related

- [JS on @moq/time](/quest/m1/time/js-migrate.md) - later moves these fake timers onto the `Manual` clock
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the same rule for tests that have already flaked
