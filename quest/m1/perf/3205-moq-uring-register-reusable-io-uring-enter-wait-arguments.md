# [S] moq-uring: register reusable io_uring_enter wait arguments

## Goal

Implement and verify the behavior tracked in [#3205](https://github.com/moq-dev/moq/issues/3205)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2875 and the CQ batching experiment in #3200.

Timed parking currently builds a `Timespec` and `SubmitArgs` for each wait, then asks the kernel to copy the extended enter arguments. Linux 6.13 added registered wait regions and `IORING_ENTER_EXT_ARG_REG`, allowing a ring to reuse kernel-known wait storage.

#### Proposal

- After #3200 fixes the wait shape, register one stable `io_uring_reg_wait` entry per worker.
- Update its absolute deadline, batch target, and minimum wait before each enter, then use `IORING_ENTER_EXT_ARG_REG`.
- Keep the current extended-argument path as the Linux 6.12 fallback. Do not raise the kernel floor for this micro-optimization without benchmark evidence.
- Prefer safe upstream support in the `io-uring` crate. If the current release lacks the registration API, contribute or upgrade that support instead of spreading raw ABI calls through the worker.
- Keep registration ownership and unregistration in one worker-owned type.
- Test repeated deadline changes, no-deadline waits, interrupted enters, and teardown.

#### Acceptance

Measure this after the winning #3200 configuration, where enter frequency and argument shape are known. Record cycles and instructions per enter, enters per second, relay CPU, and latency. Retain the 6.13 fast path only if the end-to-end CPU change is measurable.

## Required

- [#3200: moq-uring: batch completion wakeups with MIN\_TIMEOUT](/quest/m1/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) - complete the prerequisite issue first

## Closes

- [#3205](https://github.com/moq-dev/moq/issues/3205) - close this issue when the quest finishes
