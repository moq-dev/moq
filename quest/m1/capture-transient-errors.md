# [S] A transient getUserMedia failure spends the retry budget

## Goal

A camera or microphone that fails to open for a transient reason (device busy,
aborted) retries within the existing attempt budget, while a permission,
security, constraint, or missing-device refusal stays terminal at once.

## Plan

Since #3934, `js/publish/src/source/camera.ts` and
`js/publish/src/source/microphone.ts` call `retry.terminal()` for every
rejection, so `NotReadableError` is never retried and the doc in
`js/publish/src/source/retry.ts` is stale.

Decided (2026-10-04): classify by `error.name`. `NotAllowedError`,
`SecurityError`, `OverconstrainedError`, and `NotFoundError` are terminal;
anything else calls `retry.failed()`, and `out.error` is set once the budget
is spent. This spends the budget that already exists; it adds no new retry or
timeout. Fix the `retry.ts` doc.

Tests: a mocked `NotReadableError` followed by success opens the device; a
`NotAllowedError` is terminal on the first attempt.

## Closes

- [#4789](https://github.com/moq-dev/moq/issues/4789) - close this issue when the quest finishes
