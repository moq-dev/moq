# [S] One enter per turn

## Goal

A drive-loop turn that parks costs one `io_uring_enter`, not two, and a
submit never leaves deferred completions unflushed. `/metrics` shows turns,
SQEs per enter, and how many enters were forced by a full submission queue,
so the amortization the runtime exists for is a measured number rather than
a ratio of two totals.

## Plan

Branch from dev. The loop in `Worker::block_on`
(rs/moq-uring/src/worker.rs:164-187) runs one task pass, then `pump`
(`submit()` at worker.rs:250, then reap and dispatch), then `maybe_park`,
whose enter (`submit_and_wait(1)` or a timed `enter(to_submit, 1,
GETEVENTS|EXT_ARG|ABS_TIMER)`, worker.rs:400-417) carries whatever the SQ
holds. When nothing is `NOTIFIED`, the turn pays both. tokio-uring, monoio,
glommio, and compio all fold the tick's submit into the park enter.

- `pump` submits only when the turn will not park: when the unpark word is
  `NOTIFIED`, or when the SQ is full enough that the park enter could not
  carry it. Otherwise the staged SQEs ride `maybe_park`'s enter. Reaping
  before the park stays as it is; it needs no enter.
- `DEFER_TASKRUN` (set at worker.rs:104-109) runs deferred completion work
  only on an enter carrying `GETEVENTS`. The `io-uring` crate's `submit()`
  adds that flag only when waiting or on CQ overflow
  (io-uring-0.7.14/src/submit.rs:195) and never consults
  `IORING_SQ_TASKRUN`, which liburing does. Reproduce first: a worker that
  self-wakes every turn (a deep egress backlog does) and a completion that
  arrives mid-burst; measure how many turns pass before it dispatches. Then
  set `GETEVENTS` on any submit while `SubmissionQueue::taskrun()` reports
  work, and add the regression test. The inline submit in `Shared::push`
  (rs/moq-uring/src/shared.rs:111) and `submit_teardown` get the same flag.
- Metrics (rs/moq-uring/src/metrics.rs): `turns`, `sq_full_enters`, and a
  fixed-bucket histogram of SQEs per enter and CQEs per enter, one row per
  worker. `enters` stays for the ratio the docs already describe.

Acceptance: enters per turn on the chat and fanout shapes via `just bench
BASE` on Linux and the new counters; the deferred-completion regression
fails without the flag. Latency must not regress.

## Related

- [#3200](/quest/m1/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) -
  the wait side of the same enter
- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - fewer turns per
  packet, which multiplies this saving
