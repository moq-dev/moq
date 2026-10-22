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
- **qmux micro-bench.** A Criterion target in `rs/qmux/benches` over loopback
  TCP, swept over sessions × record size (the fan-out rule in `AGENTS.md`),
  for tcp, tls, ws, and wss. Use the same CPU-time mode as moq-net's
  `session` bench, not wall clock.

Record the attribution in the epic README: shares for tungstenite, the
split/pump/channel/mutex hops, per-record allocations, and TLS. Those shares
decide whether the [codec](/quest/m2/perf/stream/codec.md) goes ahead. The
[ablation](/quest/m2/perf/stream/ablation.md) adds a ring backend to this
same harness.
