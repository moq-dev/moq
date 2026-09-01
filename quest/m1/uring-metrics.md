# [M] Per-worker io_uring counters at /metrics

## Goal

The relay's internal ops listener reports what each io_uring worker is doing.
Per-session traffic already flows (the uring accept path attaches
`with_stats`), but nothing anywhere reports the runtime's own health, so the
failure modes the `moq-uring` README calls out are invisible in production and
the perf quests have no live number to be judged against.

Prometheus only. The counters are runtime internals, not traffic, so they do
not belong on the moq-stats wire.

## Plan

`moq-uring` exposes a counter snapshot per worker (through the existing
`Handle`, which already crosses threads), and `moq-relay`'s `render_metrics`
emits them beside the traffic counters and accept health it already renders.
Hand-formatted like the rest of that function; no metrics-registry dependency.

Four groups, all cheap relaxed atomics on the worker thread:

- **Buffer-pool health**: provided-buffer exhaustion, `ENOBUFS`, TX-pool
  stalls. Receive-side exhaustion *is* the backpressure, so it is the first
  thing to look at when throughput sags.
- **Batch effectiveness**: GRO segments per receive, GSO segments per send,
  packets per syscall. M4's exit criteria asked that receive batching and send
  GSO stay "observable"; this is what makes that true off the bench.
- **Ring traffic**: submissions, completions, `io_uring_enter` calls. The
  syscall reduction the runtime exists for, and the metric
  [#3199](/quest/m1/perf/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md)
  and [#3200](/quest/m1/perf/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md)
  are judged by.
- **Scheduling**: park/wake counts, timer heap depth, timers armed/fired/
  cancelled. Timer churn and futex wakes were open risks in the original epic,
  and the clock-read work in
  [#3122](/quest/m1/perf/3122-moq-uring-2-5-of-relay-cpu-is-vdso-clock-reads-the-drive.md)
  needs the baseline.

Per cross-package sync, update the `/metrics` documentation in
`doc/bin/relay/http.md`. Cover the renderer with the exposition-format test
that already guards the existing counters.

## Related

- [qlog](/quest/m1/uring-qlog.md) - the trace half of io_uring-mode
  observability
