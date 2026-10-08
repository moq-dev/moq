# [S] One enter per turn

## Goal

A drive-loop turn that parks costs one `io_uring_enter`, not two, and a
submit never leaves deferred completions unflushed. `/metrics` shows turns,
SQEs per enter, and how many enters were forced by a full submission queue,
so the amortization the runtime exists for is a measured number rather than
a ratio of two totals.

## Plan

Branch from main. The loop in `Worker::block_on`
(`rs/moq-uring/src/worker.rs`) runs one task pass, then `pump` (`submit`,
then reap and dispatch), then `maybe_park`, whose enter
(`submit_and_wait(1)` or a timed `enter(to_submit, 1,
GETEVENTS|EXT_ARG|ABS_TIMER)`) carries whatever the SQ holds. When nothing is `NOTIFIED`, the turn pays both. tokio-uring, monoio,
glommio, and compio all fold the tick's submit into the park enter.

- `pump` submits only when the turn will not park: when the unpark word is
  `NOTIFIED`, or when the SQ is full enough that the park enter could not
  carry it. Otherwise the staged SQEs ride `maybe_park`'s enter. Reaping
  before the park stays as it is; it needs no enter.
- `DEFER_TASKRUN` (set in `Worker::new`) runs deferred completion work
  only on an enter carrying `GETEVENTS`. The locked `io-uring` 0.7.15's
  `Submitter::submit_and_wait` already adds that flag while
  `IORING_SQ_TASKRUN` is set, as liburing does; 0.7.14, which the workspace
  still allows, does not. Raise the workspace floor to 0.7.15 and pin it with
  a regression test: a worker that self-wakes every turn (a deep egress
  backlog does) dispatches a completion that arrives mid-burst within one
  turn. A submit this quest folds into the park enter must keep the flag.
- Metrics (rs/moq-uring/src/metrics.rs): `turns`, `sq_full_enters`, and a
  fixed-bucket histogram of SQEs per enter and CQEs per enter, one row per
  worker. `enters` stays for the ratio the docs already describe.

Decided in the 2026-10-05 audit: driver-touching perf work waits on the
[hard fork](/quest/m1/quic/fork/README.md). This quest's worker loop,
`Shared::push`, and metrics changes do not touch the QUIC driver
(`rs/moq-uring/src/quic/noq`), so it can land first; any part that turns out
to edit the driver moves to a follow-up after the fork.

Acceptance: enters per turn on the chat and fanout shapes via `just bench
BASE` on Linux and the new counters; the deferred-completion regression
fails on io-uring 0.7.14. Latency must not regress.

## Related

- [Run to quiescence](/quest/m1/perf/uring-quiescence.md) - fewer turns per
  packet, which multiplies this saving
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) -
  the wait side of the same enter
