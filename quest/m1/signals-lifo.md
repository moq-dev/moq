# [M] Signals effects clean up last-in, first-out

## Goal

`@moq/signals` runs an effect's cleanups in reverse registration order, like
`DisposableStack` and Rust drop order, so a resource registered later (a
nested connection) is torn down before the one it depends on. Audio capture
teardown then logs nothing.

## Plan

`#drain` in `js/signals/src/index.ts` runs cleanups first-in, first-out.
`js/publish/src/audio/capture.ts` registers `root.disconnect()` before the
nested effect's `root.disconnect(worklet)`, so the nested one throws
`InvalidAccessError` and signals logs "cleanup error" on every teardown.

Decided (2026-10-04): fix the order at its source rather than deleting the
one line. Audit every effect in `js/` for a dependence on first-in, first-out
order, and fix what the change breaks in the same PR. Make the audio test
fake's `disconnect(node)` throw for an unconnected node, as browsers do, so
the capture case is covered. Document the order on `Effect.cleanup`.

Tests: a signals unit test registers three cleanups and asserts reverse
order, including nested effects; capture teardown with the strict fake logs
nothing.

## Closes

- [#4788](https://github.com/moq-dev/moq/issues/4788) - close this issue when the quest finishes
