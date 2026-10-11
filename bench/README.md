# Benchmarks

This directory contains repository-level benchmark orchestration. Rust
microbenchmarks stay beside their crates under `rs/*/benches`, while the
`moq-bench` load generator and host sampler stay in `rs/moq-bench`.

`run.sh` owns builds, comparison rounds, and reporting. `relay.sh` owns the
temporary relay lifecycle and load execution shared by each comparison mode.

## Commands

Run every Criterion target plus the local relay workloads:

```bash
nix develop --command just bench
```

Measure 50 fps audio at per-packet, 100 and 200 ms grouping, plus 400 fps
(2.5 ms packets) at per-packet and the 20 ms default. Frames are 200 bytes:

```bash
nix develop --command just bench-audio
```

This sweeps room connections (16, 32) and subscriptions per connection (2, 8)
independently, plus one publisher serving 64 and 200 subscribers. It uses the
normal relay sampler and runs nightly. Each shape reports delivered frames,
p99 latency, relay CPU and RSS; the group-size suffix is the number of frames
after the keyframe (0, 4, 9), so each group carries 1, 5 or 10 frames. These
synthetic payloads measure group overhead; they do not measure codec loss
concealment.

Compare the current tree with another revision:

```bash
nix develop --command just bench origin/main
```

Compare one multi-threaded Tokio runtime with the same number of independent
Tokio/epoll and io\_uring workers:

```bash
nix develop --command just bench-runtime
nix develop --command just bench-runtime 5 16
```

Runtime comparison requires Linux because io\_uring and relay process metrics
come from Linux interfaces. The default worker count is the number of online
logical CPUs.

## Workloads

The `workloads/` TOML files contain only traffic shape. The harness supplies the
temporary relay URL, TLS settings, startup ramp, run duration, reporting
interval, and output paths so every runtime receives the same load.

- `video`: light many-to-many video traffic.
- `fanout`: light one-to-many traffic.
- `video-heavy`: multicore many-to-many video traffic.
- `fanout-heavy`: multicore one-to-many traffic near saturation.
- `audio` and `audio-fanout`: small, 50 fps audio-shaped frames; `bench-audio`
  sweeps grouping and load shape through command-line overrides.

The runtime matrix rotates execution order between rounds, then reports the
median throughput, loss, latency, CPU split, context switches, RSS, and thread
count. Compare CPU only when delivered throughput and loss are equivalent. A
runtime that falls behind can use less CPU simply because it completed less
work.

Benchmark output is informational and machine-specific. Crashes, zero delivery,
and invalid samples still fail the command.


BASE comparisons run paired rounds (default three, `MOQ_BENCH_RUNTIME_ROUNDS`),
alternating base/current order. Startup remains outside the final five-second
throughput window. `MOQ_BENCH_ARTIFACTS` chooses the evidence parent (default
`target/bench/runs`); each invocation retains a new run directory even on failure.
It contains raw load/host JSONL, process diagnostics, Criterion statistics,
execution order, source revision and tracked dirty patch, tool/build/machine
metadata, and workload files. Untracked files are not part of the recorded patch.

Each comparison reports the median of the paired percentage deltas, their median
absolute deviation and range, and flags fewer than three observations or a sign
that varies across pairs. MAD is descriptive dispersion, not a confidence
interval; three rounds cannot establish a small improvement. Timing remains
informational. Loss and latency are explicitly lifetime statistics; throughput
and host CPU carry their own observed window bounds. CPU efficiency is withheld
until offered load, delivery/loss and those window bounds are comparable. The
load generator shares the loopback host, so generator saturation can hide a relay
improvement; these reports do not yet quantify generator CPU or worker metrics.
