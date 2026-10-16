# Benchmarks

This directory contains repository-level benchmark orchestration. Rust
microbenchmarks stay beside their crates under `rs/*/benches`, while the
`moq-bench` load generator and host sampler stay in `rs/moq-bench`.

`run.sh` owns builds, comparison rounds, and reporting. `relay.sh` owns the
temporary relay lifecycle and load execution shared by each comparison mode.
A temporary relay is ready only once its PID owns both the TCP listener and UDP
socket on its assigned port. `lsof`, included in the Nix shell, verifies
ownership. A collision uses the existing startup attempts; exhausted attempts
fail with the relay log before any load starts.

## Commands

Run every Criterion target plus the local relay workloads:

```bash
nix develop --command just bench
```

Measure small-group session cost over independent publisher and viewer counts,
for SUBSCRIBE and FETCH:

```bash
nix develop --command cargo bench --locked -p moq-net --bench session -- session_delivery_group_cost
nix develop --command env SESSION_CPU=1 cargo bench --locked -p moq-net --bench session -- session_delivery_group_cost_cpu
```

The CPU mode requires Unix and measures the benchmark's current thread, including
its protocol drivers and the flow-control fixture but excluding scheduler
preemption. Both modes exclude setup and cache-clock advancement. Constrained
cases hold data after the first transport write and release it only after a
write actually blocks; Lite FETCH uses a bidi response, while IETF FETCH and
SUBSCRIBE use uni data streams. No sleep supplies backpressure. Every case
checks complete payload delivery. Compare paired runs with identical settings
and disclose host load; CPU timing still depends on hardware contention.

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

Sweep the relay's QUIC shard count against the connection count, for the
room (`video`: every connection publishes and subscribes) and `fanout` (one
publisher, the rest subscribers) workloads:

```bash
nix develop --command just bench-shard
nix develop --command just bench-shard "0 1 8 24" "64 512"
```

Zero shards is the shared runtime with one Tokio thread per logical CPU; any
other count is that many unpinned Tokio workers. The table reports CPU per
connection, so a steering, dispatch, or per-shard cost that grows with either
axis reads as a slope. Linux only, since nothing else steers a reuseport
group. It runs nightly.

Attribute relay worker lock waits to the stacks that blocked, as a share of
each worker's CPU:

```bash
nix develop --command just bench-lock
nix develop --command just bench-lock fanout-heavy "4 16"
```

This builds the relay with `[profile.profiling]` and frame pointers, then runs
the workload on Tokio workers at each count twice: an unprofiled control, then
a profiled run, so the throughput and CPU difference shows the profiler's
overhead. During the profiled run's steady window, `perf record` samples every
worker switch-out (the `context-switches` event with a period of 1) with a
DWARF-unwound stack, plus each switch-in, so every off-CPU interval has an
exact duration. glibc's `syscall` wrapper keeps no frame pointer, so
frame-pointer unwinding alone would drop `Mutex::lock_contended`; without
frame pointers in the relay, perf's DWARF unwinder stops at the first relay
frame. Both are needed. An interval counts as a mutex wait only when the worker is blocked in
the futex under std's `Mutex::lock_contended` (`kio::Lock`). Condvar,
allocator and parking waits are counted separately, and spinning is excluded.
CPU runs from each switch-in to the sampled switch-out, where the next wait
begins, clipped to the same recorded window as waits.
`/proc` schedstat only identifies workers that slept throughout: a silent worker
whose counter advanced is ambiguous and refused. A worker with switch records
but no sampled switch-out is refused too. `lock.json` version 3 uses this
recorded CPU denominator; wait/CPU can exceed one.
`lock/mutex.folded` holds the blocking stacks, weighted by microseconds
waited, in flamegraph folded format.

Linux only. A switch-out is sampled in kernel context, and 16 workers need a
larger ring buffer than the unprivileged locked-memory limit allows, so the
recipe refuses before building until `perf_event_paranoid` is -1. A recording
that still drops events is refused rather than reported.

```bash
sudo sysctl -w kernel.perf_event_paranoid=-1
```

## Workloads

The `workloads/` TOML files contain only traffic shape. The harness supplies the
temporary relay URL, TLS settings, startup ramp, run duration, reporting
interval, and output paths so every runtime receives the same load.

- `video`: light many-to-many video traffic.
- `fanout`: light one-to-many traffic.
- `video-heavy`: multicore many-to-many video traffic.
- `fanout-heavy`: multicore one-to-many traffic near saturation.
- `chat`: one broadcast to 2,000 viewers, one small message per group.
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
`target/bench/runs`), which must be outside the repository, or git-ignored and
outside `bench/`; each invocation retains a new run directory even on failure.
It contains raw load/host JSONL, process diagnostics, Criterion statistics,
execution order, source revision and tracked dirty patch, tool/build/machine
metadata, workload files, complete stdout/stderr, and a separate copy of untracked
source files.

Each comparison reports the median of the paired percentage deltas, their median
absolute deviation and range, and flags fewer than three observations or a sign
that varies across pairs. MAD is descriptive dispersion, not a confidence
interval; three rounds cannot establish a small improvement. Timing remains
informational. Loss and latency are explicitly lifetime statistics; throughput
and CPU use the same load-window boundaries. On Linux, one host sampler observes
both the relay and the generator, then stops the generator after the measurement
ends. CPU counters are linearly interpolated between the surrounding 500 ms
samples; this is a sampled estimate, not an exact accounting boundary. Raw
per-thread samples and CPU rates for threads present at both boundaries are
retained; short-lived threads remain represented in process CPU totals.

Generator CPU is reported beside relay CPU because sharing a loopback host can
hide a relay improvement behind generator saturation. CPU efficiency remains
withheld until offered load and delivery/loss are comparable. Zero-baseline
metrics retain absolute deltas when their percentage ratio is undefined.

Measure what each binding adds over Rust:

```bash
nix develop --command just bench-ffi
```

Every runner (Rust, Go, Python, Kotlin) runs the same in-process rounds with no
network: each publisher writes one group from the driving thread while every
subscriber reads it on its own task, and a round waits for every subscriber. It
sweeps frame sizes from 100 B to 1 MiB at one publisher and subscriber, then
1/4 publishers by 1/4 subscribers at 1 KiB, and times two sync calls
(`sequence`, `info`). Each case reports delivered MB/s and process CPU (user +
system, every thread) per MB. `MOQ_BENCH_SECONDS` sets each case's time budget
(default 1); every received frame is checked byte for byte.

Each runner writes `target/bench/ffi/<runner>.json`; `bench/ffi.py` combines
them into a table and `report.json`. Ratios are against `rust-ffi`, moq-ffi's
API called from Rust without crossing the FFI, so a wrapper's ratio is what its
boundary and wrapper cost. `rust-net` runs the same rounds on moq-net, so its
ratio is what moq-ffi's handles and runtime cost. Ratios hold only within one
machine. The nightly runs it, prints the table in the job summary, and keeps the
results as the `bench-ffi` artifact. Each language also has its own recipe
(`just go bench`, `just py bench`, `just kt bench`).

## Pattern and container coverage

Rust Criterion coverage includes `moq-pattern`'s
`matching` (pattern count and path depth, last match and miss) and `moq-mux`'s
`containers` (CMAF and Annex-B import/export). Container cases vary frame count
and payload size; CMAF also varies track count. Separate frame and byte rates
use the same fixtures, with byte rates counting the encoded stream size.
Fixture generation, publisher setup and destruction stay outside timing.
The existing shared Rust benchmark nightly invocation runs both targets; their `--test` mode
exercises every case without making timing claims.
