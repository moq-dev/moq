# [XS] Confirm io_uring quiescence on a quiet host

## Goal

Run-to-quiescence on the io_uring workers has acceptance numbers from a quiet host (fanout CPU per Gbps and p99 from `just bench-runtime`, against the tokio workers).

## Plan

Follow-ups of [moq-dev/moq#7530](https://github.com/moq-dev/moq/pull/7530), which runs a turn's tasks to quiescence before submitting. Its deterministic counts were the evidence (fanout 40.2 to 20.9 turns per round, -3.6% instructions); its `just bench-runtime` rows were indicative only, because host load put every runtime's p99 in seconds and moved the unchanged tokio rows by up to 8%. #7530 merged 2026-11-02.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, one quest for both.

- **Quiet re-run.** Record `/proc/loadavg` and run `just bench-runtime` for fanout and fanout-heavy, base against [moq-dev/moq#7530](https://github.com/moq-dev/moq/pull/7530), alternating. [moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533) and [moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534) saw the load generator's p99 sit near 1.4 s in every run, a startup artifact inside the throughput window, so check whether a quiet host alone fixes p99 or the generator's latency window needs to exclude startup first. Record the result in the [perf epic](/quest/m2/perf/README.md).
The QUIC request-reply benchmark is now part of `worker_turns` in
[moq-dev/moq#7530](https://github.com/moq-dev/moq/pull/7530). The nightly
`rust-shared` group retains its timing series and turn/enter counter deltas in
the run logs; adding that benchmark is no longer open work here.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - sets the threshold the new series is judged by
