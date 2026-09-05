# [M] Reproducible relay CPU and allocation profiles

## Goal

One local recipe captures a symbolized relay profile under an existing workload,
with enough metadata to reproduce it and distinguish relay cost from load-generator
cost. Profiling is opt-in and has no production overhead when disabled.

## Plan

`rs/scripts/bench.sh` already owns the builds, relay PID, workload, and host
samples, but has no profiler integration. Reuse that lifecycle instead
of adding a second launcher. `Cargo.toml` already has a `profiling` profile and
`rs/moq-native/src/jemalloc.rs` already supports on-demand heap dumps.

- Add a focused `just` recipe selecting workload, duration, and capture mode through
  one configuration. Reuse locked builds and the existing profiling Cargo profile;
  record frame-pointer flags, features, compiler, OS/kernel, CPU, runtime, affinity,
  allocator, exact revision, dirty diff, and resolved workload alongside the result.
- Capture the relay process and all its workers, excluding the load generator.
  Start sampling after readiness and mark the measurement window. Keep an
  uninstrumented control run to quantify profiler overhead.
- Support symbolized CPU stacks on Linux with perf and on macOS with a maintained
  profiler such as samply. Probe tool/platform permissions and report unsupported
  captures clearly. Do not silently substitute wall-clock timing for CPU samples.
- Reuse jemalloc's supported build/configuration and signal listener for heap
  snapshots before load, at steady state, and after teardown. Include allocation
  churn as well as retained bytes; snapshots alone cannot establish allocation rate.
  Never send a profiling signal before verifying the listener is active.
- Retain raw captures, symbols or binary identity, commands, logs, and a locally
  viewable report in a caller-selected artifact directory. On cancellation or a
  failed capture, stop owned profiler/load/relay children and preserve diagnostics.
- Validate on one supported Linux host and macOS. A smoke capture must contain
  resolved MoQ frames, the intended PID/window, and nonempty samples. Exercise an
  unavailable profiler and an interrupted run without leaving children behind.

Tool references: [samply](https://github.com/mstange/samply) and the existing
[jemalloc controls](https://jemalloc.net/jemalloc.3.html) are starting points;
verify current supported versions and pin any newly installed tools.

## Related

- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - repeatable results and artifact metadata
- [Traffic counter contention](/quest/m2/stats-contention.md) - a concrete CPU attribution experiment
- [Relay memory](/quest/m2/relay-memory.md) - retained route and announcement memory
