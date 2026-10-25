# [M] Stream transport bench

## Goal

The relay's stream transports have a measured baseline. `just bench` runs
its relay workloads over `ws://`, `wss://`, `tcp://`, and `tls://` qmux as well
as QUIC.
A qmux micro-bench attributes CPU to the layers the
[epic](/quest/m2/perf/stream/README.md) names, so each later quest is judged
against a number rather than intuition.

## Plan

Two parts. Both are needed: the relay run includes axum, hyper, and rustls,
and the micro-bench isolates qmux.

- **Relay workloads.** `bench/relay.sh` dials `https://` only today. Add a
  transport axis to the existing `chat` (F=0, per-message cost) and `fanout`
  (per-byte cost) workloads in `bench/workloads/`: `ws://` and `wss://`
  through moq-bench's existing WebSocket options, and `tcp://` and `tls://`
  qmux against the relay's stream listeners. Report CPU
  per message and per Gbps in the same machine-readable form
  `bench/compare.py` already reads. `just bench-profile` then gives the
  flamegraph per transport.
- **qmux micro-bench.** A local profiling tool, not a regression series:
  [transport sweep](/quest/m2/bench-transport-sweep.md) owns qmux's series
  (decided 2026-10-22, maintainer). An example binary in `rs/qmux/examples`,
  not a Cargo bench target, since Nightly runs and reports every bench target
  (`sh/gh/nightly.test.py`). It drives loopback TCP, swept over sessions ×
  record size (the fan-out rule in `AGENTS.md`), for tcp, tls, ws, and wss,
  and reports process CPU time, not wall clock, so it runs under a profiler.

Record the attribution in the epic README: shares for tungstenite, the
split/pump/channel/mutex hops, per-record allocations, and TLS. Those shares
decide whether the [codec](/quest/m2/perf/stream/codec.md) goes ahead. The
[ablation](/quest/m2/perf/stream/ablation.md) adds a ring backend to this
same harness.

## Related

- [Transport x version benches](/quest/m2/bench-transport-sweep.md) - adds qmux as a transport axis of the moq-net session bench
