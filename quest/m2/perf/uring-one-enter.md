# [S] One enter per turn

## Goal

A drive-loop turn that parks costs one `io_uring_enter`, not two, and a
submit never leaves deferred completions unflushed. `/metrics` shows turns,
SQEs per enter, and how many enters were forced by a full submission queue,
so the amortization the runtime exists for is a measured number rather than
a ratio of two totals.

## Plan

The worker placement, deferred-work fix, dependency floor, and observations
are implemented on `quest/m2/perf/uring-one-enter`. Parked-turn and
already-armed/full-SQ regressions fail with two enters and pass with one.
The busy-turn regression fails with an empty-SQ early return and with
io-uring 0.7.14; it passes with TASKRUN_FLAG and io-uring 0.7.15. Existing
Snapshot counters stay, with `turns`, `sq_full_enters`, and nested `sqes` and
`cqes` histograms added.

The Worker benchmark independently sweeps 1/4/16 sockets and 1/8/64 datagrams
per burst. All nine measured means were unchanged or lower (0.1–6.2% lower)
on one pinned CPU. Three rotated relay rounds on four CPUs compared main,
main with identical observation counters, and the implementation. Chat total
enters fell 13,805 to 3,218, while turns fell 30,103 to 2,769: enters/turn alone
rises when the deferred-work fix removes empty busy turns. Median delivered
throughput and p99 did not worsen against untouched main in those rounds.

The strict end-to-end latency acceptance remains open. Fanout delivered the
same 640 fps with no loss and median p99 of 1426 ms before/1425 ms after, but its
median p50 was 2 ms before/3 ms after and relay CPU was 0.0300/0.0379 core. These
are coarse, lifetime statistics dominated by startup; current p50 varied
4/3/2 ms across rounds. Separate a real regression from startup, clock
quantization, and machine variation with steady-state evidence before
claiming the full latency goal is met. Retain the deferred-completion
regression and do not mask the question with sleeps, retries, or timeout
changes.

The retained paired run uses the existing workload lifecycle. A first run
stopped on the relay-port race (fixed by #5636);
the completed diagnostic used preflighted unique ports below the host's
client ephemeral range, with no runtime retry. The benchmark CLI is unchanged.

The completed work changes only the Worker loop, Shared::push, observations,
and their consumers, leaving the QUIC driver alone.

## Related

- [Run to quiescence](/quest/m2/perf/uring-quiescence.md) - fewer turns per
  packet, which multiplies this saving
- [#3200](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md) -
  the wait side of the same enter
