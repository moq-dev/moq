# [M] Ring TCP ablation

## Goal

A written-down number comparing ring TCP against tokio TCP under the relay's
actual stream workload, on the same hardware. It picks which ring mechanisms
the [port](/quest/m2/perf/stream/ring.md) uses and records the expected win.

## Plan

Decided 2026-10-22: this no longer gates the line. The port goes ahead, and
the ablation decides its shape.

Add a ring backend to the stream transport bench harness (the relay
workloads' transport axis in `bench/relay.sh` and the qmux micro-bench in
`rs/qmux/examples/bench.rs`) rather than building a second one. Follow the echo bench
(`rs/moq-uring/benches/session_lite.rs`): a bench over the real worker,
toggling one thing at a time, not a synthetic microbenchmark alone. Include
the `F=0` chat shape, where the cost is per message rather than per byte,
since that is where a syscall per read hurts most.

Vary at least:

- tokio (epoll) against ring
- multishot `recv` with a provided buffer ring against a read per completion
- batched writes against a write per frame
- registered fixed files against fd lookups

Record CPU per message, CPU per Gbps, syscalls per message, ring submissions
and completions, and latency, in the same machine-readable form the UDP matrix
uses. Attribute the gain (or its absence) to specific mechanisms, and record
the result in the [epic](/quest/m2/perf/stream/README.md) README.
