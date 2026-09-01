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

The runtime matrix rotates execution order between rounds, then reports the
median throughput, loss, latency, CPU split, context switches, RSS, and thread
count. Compare CPU only when delivered throughput and loss are equivalent. A
runtime that falls behind can use less CPU simply because it completed less
work.

Benchmark output is informational and machine-specific. Crashes, zero delivery,
and invalid samples still fail the command.
