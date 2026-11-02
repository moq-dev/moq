# [XS] Confirm io_uring quiescence on a quiet host and track its turn counts nightly

## Goal

Run-to-quiescence on the io_uring workers has acceptance numbers from a quiet host (fanout CPU per Gbps and p99 from `just bench-runtime`, against the tokio workers), and a QUIC request-reply variant of the `worker_turns` bench gives its turn and enter counts a nightly series, so a regression in batching shows up.

## Plan

Follow-ups of [moq-dev/moq#7530](https://github.com/moq-dev/moq/pull/7530), which runs a turn's tasks to quiescence before submitting. Its deterministic counts were the evidence (fanout 40.2 to 20.9 turns per round, -3.6% instructions); its `just bench-runtime` rows were indicative only, because host load put every runtime's p99 in seconds and moved the unchanged tokio rows by up to 8%. This waits on #7530 landing.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, one quest for both.

- **Quiet re-run.** Record `/proc/loadavg` and run `just bench-runtime` for fanout and fanout-heavy, base against #7530, alternating. [moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533) and [moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534) saw the load generator's p99 sit near 1.4 s in every run, a startup artifact inside the throughput window, so check whether a quiet host alone fixes p99 or the generator's latency window needs to exclude startup first. Record the result in the [perf epic](/quest/m2/perf/README.md).
- **Nightly series.** #7530's harness (`rs/moq-uring/examples/quiescence/`, in its PR body, uncommitted) drove chat, bulk, and fanout shapes over QUIC and counted instructions, turns, and enters per round. Add a QUIC request-reply variant to the existing `worker_turns` bench (`rs/moq-uring/benches/worker_turns.rs`, already in `sh/gh/nightly.sh`'s bench list), reporting turns and enters per round as that harness did, so the nightly series covers it.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - sets the threshold the new series is judged by
