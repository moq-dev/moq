# [S] A busy or aborted getUserMedia spends the retry budget

## Goal

A camera or microphone that fails to open because the device is busy
(`NotReadableError`) or the request was aborted (`AbortError`) retries within
the existing attempt budget. Every other rejection is terminal at once.

## Plan

Since #3934, `js/publish/src/source/camera.ts` and
`js/publish/src/source/microphone.ts` call `retry.terminal()` for every
rejection, so a device-switch race is never retried, and the doc in
`js/publish/src/source/retry.ts` is stale.

Decided (2026-10-04): an allowlist, not a denylist. Only `NotReadableError`
and `AbortError` call `retry.failed()`; anything else, including `TypeError`
from bad constraints, stays terminal (fail loud). `out.error` stays unset
while retrying and is set once the budget is spent. This spends the budget
that already exists and adds no new retry or timeout. Rust's capture makes
the same split (`Failure::retry` and `fatal`). Fix the `retry.ts` doc.

Tests: a mocked `NotReadableError` followed by success opens the device; a
`NotAllowedError` or `TypeError` is terminal on the first attempt.

## Closes

- [#4789](https://github.com/moq-dev/moq/issues/4789) - close this issue when the quest finishes
