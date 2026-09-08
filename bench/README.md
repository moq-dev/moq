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

## Frame storage experiments

Run from the repository root on the machine being measured:

```sh
cargo test --locked -p moq-net --example adaptive-frames
cargo run --locked --release -p moq-net --example adaptive-frames > adaptive-frames.csv
cargo run --locked --release -p moq-net --example frame-storage > frame-storage.csv
```

With Nix, prefix each command with `nix develop --command`.

- [A/V-sized matrix](av-frames.md): 100 B through 1 MiB, live fanout, and retained storage.
- [Production integration](adaptive-net.md): adaptive allocation in moq-net with before/after measurements.
- [Adaptive prototype](adaptive-frames.md): first-frame-sized pages, capped growth, direct `BufMut` writes, and separate versus packed headers.
- [Initial investigation](frame-storage.md): current-model measurements and isolated storage comparisons.

The programs print CSV to stdout. Run each more than once on an otherwise idle machine, and record the commit, CPU, OS, and Rust version with the results. Keep release mode enabled. Timing includes the instrumentation wrapper with counting disabled; allocation counts are collected in separate untimed passes.

Committed CSV files are historical local measurements, not expected test outputs. The initial measurements used `1e6c7ce0f` on `dev`; this PR targets `dev`. Rerunning the current-model comparison on this branch measures its own model revision, so do not label it an exact reproduction of the historical results. The example implementations are isolated; the production integration changes chunked-frame allocation without changing the wire format.
