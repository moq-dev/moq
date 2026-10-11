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

## Profiling capability draft

`nix develop --command just bench-profile-probe lock OUTPUT` probes bpftrace
permissions before a costly build; use `cpu` to probe perf's CPU clock event.
A refused probe retains stdout, stderr and its command in OUTPUT and exits
unsuccessfully. Linux lock capture needs sudo or CAP\_BPF plus tracefs access;
CPU capture needs perf\_event permission. The lock probe loads both futex tracepoints and user-stack collection before exiting; a BEGIN-only program would not establish that those facilities are usable. Missing tools and process-start failures retain refusal diagnostics too. The probe captures no profile.

`profile.py thread-cpu PID OUTPUT` records relay worker CPU nanoseconds from
Linux schedstat. The reducer accepts normalized records with `tid`, `comm`,
`wait_ns` and symbolized `stack` fields, plus before/after CPU counters keyed
by TID. Only std mutex `lock_contended` frames contribute to the lock share;
allocator, condvar and runtime parking waits are counted separately. The
denominator is scheduled CPU time, so wait/CPU can exceed one. Missing CPU or
user stacks and address-only unresolved stacks are refused. Spinning does not reach the futex and is excluded.

This is a partial foundation, not the profiling launcher. Still required:
integrate captures with run.sh's owned-child lifecycle, preserve metadata and
control runs, normalize bpftrace output, validate resolved MoQ stacks, capture
CPU profiles and jemalloc snapshots/churn, and exercise macOS samply. Heap
capture must verify the existing SIGUSR1 listener before sending any signal.
