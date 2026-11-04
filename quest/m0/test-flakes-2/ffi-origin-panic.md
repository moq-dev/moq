# [S] moq-ffi's panicked-origin supervision test survives load

## Goal

`moq-ffi origin::supervision_tests::a_panicked_origin_task_reaches_the_host_while_another_origin_keeps_working`
(`rs/moq-ffi/src/origin.rs`) passes reliably in a loaded merge-queue
`just ci`, fixed at its cause, never by raising a timeout or adding a retry.
Done when the test no longer fails the queue and a loaded local repeat of it
passes.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). It failed the merge-queue
batch for [moq-dev/moq#7771](https://github.com/moq-dev/moq/pull/7771)
([run 41752982583](https://github.com/moq-dev/moq/actions/runs/41752982583),
2026-11-04 01:17Z) after 0.070 s:

```text
FAIL [   0.070s] ( 2086/11649) moq-ffi origin::supervision_tests::a_panicked_origin_task_reaches_the_host_while_another_origin_keeps_working
thread 'moq-ffi-worker' (1035695) panicked at rs/moq-ffi/src/ffi.rs:558:17:
injected origin driver panic
thread 'origin::supervision_tests::a_panicked_origin_task_reaches_the_host_while_another_origin_keeps_working' (1035639) panicked at rs/moq-ffi/src/origin.rs:688:9:
assertion failed: matches!(failed.next().await, Err(MoqError::Task { .. }))
```

The injected panic fired on the worker (`panicking_origin` in
`rs/moq-ffi/src/ffi.rs`), but the pending `announced().next()` returned
something other than `MoqError::Task`. That matters beyond the test: a host
waiting on announcements can miss the origin task's failure, which is the
behavior the test exists to prove.

- Find what `next()` returned instead: most likely the origin closing (an end
  or `Closed`) reached the read before the driver's panic was recorded as the
  task's failure.
- If so, fix the order in the driver supervision, so a read the panic ends
  always reports `Task`. Don't loosen the assertion.
- [moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752) edits
  `rs/moq-ffi/src/ffi.rs`. Start from whichever version is on main.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
