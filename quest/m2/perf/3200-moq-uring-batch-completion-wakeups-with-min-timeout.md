# [M] moq-uring: batch completion wakeups with MIN_TIMEOUT

## Goal

`maybe_park` (rs/moq-uring/src/worker.rs:363) waits with a CQE batch target
and a `min_wait_usec`, the three return paths are tested, and the benchmark
matrix shows fewer wakeups per message without a p99 regression before any
default changes.

## Plan

Follow-up to #2875.

The worker requires `IORING_FEAT_MIN_TIMEOUT`, but `maybe_park` currently waits for one completion and does not set `min_wait_usec`. The kernel therefore wakes the thread for each first CQE even when a few microseconds of coalescing could amortize enter, CQ, and task-dispatch overhead.

#### Mechanism

A timed `io_uring_enter` can combine:

- a CQE batch target `N`
- a short minimum wait `t` once at least one CQE exists
- the next application timer `T` as the absolute maximum wait

The kernel returns when `N` CQEs arrive, when `t` expires after partial progress, or when `T` expires with no progress. This should trade a bounded amount of latency for fewer wakeups and larger batches.

#### Proposal

- Add benchmark knobs for the CQE batch target and minimum wait.
- Apply them only when the worker is otherwise ready to park. Preserve the next timer as the hard deadline.
- Measure a small static matrix first, then consider an adaptive target based on recent CQ batch occupancy.
- Keep remote futex wakes and handshake/control traffic bounded by the configured minimum wait.
- Test all three return paths: batch target reached, partial batch released by the minimum wait, and empty ring released by the application deadline.

#### Acceptance

Benchmark chat, 1:1 video, and fanout workloads with `N = 1/4/8/16` and `t = 0/5/10/20 us`. Record CQEs per wake, enters per second, CPU per message, p50, p99, and p999 latency. Pick no production default until the latency budget and CPU win are both demonstrated.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#3200](https://github.com/moq-dev/moq/issues/3200) - close this issue when the quest finishes
