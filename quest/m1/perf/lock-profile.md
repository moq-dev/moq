# [M] One relay profiling recipe

## Goal

One local `just` recipe runs a bench shape and captures a relay profile in
one of three modes, with enough metadata to reproduce it and to tell relay
cost from load-generator cost:

- **Lock wait** (first): bpftrace reports, per relay worker thread
  (`moq-uring-N`, `moq-quic-N`), the time spent blocked in futex waits and the
  user stacks that blocked, as a share of that thread's CPU. Lock wait becomes
  a number anyone can reproduce without code in `kio`.
- **CPU**: symbolized CPU stacks, with perf on Linux and samply on macOS.
- **Heap**: jemalloc heap snapshots and allocation churn.

Profiling is opt-in and costs nothing when disabled.

## Plan

Decided 2026-10-08: the reproducible CPU and allocation profile quest merged
here, so the relay has one profiling recipe with capture modes rather than
two launchers. The lock-wait mode lands first, since
[kio channel contention](/quest/m1/perf/kio-channel-contention.md) waits on
it; the CPU and heap modes follow in the same recipe.

Shared by every mode:

- Reuse `bench/run.sh`'s lifecycle (builds, relay PID, workload, host
  samples) rather than adding a second launcher. Select workload, duration,
  and mode through one configuration.
- Build the relay with `[profile.profiling]` and
  `RUSTFLAGS="-C force-frame-pointers=yes"`: the release build `bench/run.sh`
  uses has neither debug info nor frame pointers, so stacks would stop at the
  libc frame.
- Capture the relay process and all its workers, never the load generator.
  Start after readiness and mark a steady window. Keep an uninstrumented
  control run to quantify profiler overhead.
- Record the revision and dirty diff, build flags, features, compiler,
  OS/kernel, CPU, runtime, affinity, allocator, and resolved workload with the
  result, in a caller-selected artifact directory. On cancellation or a failed
  capture, stop every owned child and keep the diagnostics.
- Local only, documented next to `just bench` and `just bench-runtime`. Not
  in CI, since the modes need privileges. Probe tool and permission support
  and refuse an unsupported capture clearly; never substitute wall-clock
  timing for CPU samples.

Lock wait (decided by the maintainer 2026-10-07, after closing #5031, which
timed every contended `kio::Lock` acquire in code and added public API in
three crates plus five Prometheus families):

- Measure from outside the process: bpftrace on the futex syscalls of the
  relay's worker threads, aggregated by thread name and user stack. No
  instrumentation in `kio`, no new public API, no metrics.
- `Consumer::poll` is generic and inlined, and bpftrace doesn't expand inline
  frames, so the stacks name its callers.
- Count only stacks under the std mutex's contended path (`kio::Lock` is a
  `std::sync::Mutex`): futex waits also include allocator, condvar, and tokio
  waits, and a contended acquire that resolves while spinning never reaches
  the futex.
- Add bpftrace to the Nix dev shell for Linux only
  (`lib.optionals stdenv.isLinux`), since the shell also builds for
  `aarch64-darwin`. The mode needs sudo (or `CAP_BPF`) and says so.
- Shapes and worker counts follow #5031: fanout (`bench/workloads/fanout.toml`)
  and chat (`rs/moq-bench/config/chat.toml`) at 1, 4, and 16 workers. Wait is
  off-CPU, so a thread's share can exceed 1 when it blocks longer than it runs.
- Check that it roughly reproduces #5031's wait time (see kio channel
  contention), not its contended-acquire counts, and that its stacks lead to
  the same call sites.

CPU and heap:

- CPU stacks with perf on Linux and a maintained profiler such as
  [samply](https://github.com/mstange/samply) on macOS; pin any newly
  installed tool.
- Heap snapshots through `rs/moq-tokio/src/jemalloc.rs`'s existing
  on-demand dumps, before load, at steady state, and after teardown. Include
  allocation churn as well as retained bytes, since snapshots alone cannot
  establish an allocation rate. Never send a profiling signal before
  verifying the listener is active.
- Validate on one Linux host and macOS: a smoke capture holds resolved MoQ
  frames, the intended PID and window, and nonempty samples. Exercise an
  unavailable profiler and an interrupted run without leaving children
  behind.

## Related

- [kio channel contention](/quest/m1/perf/kio-channel-contention.md) - requires this for its before and after
- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - shares the workload lifecycle and artifact conventions
- [Release profile](/quest/m1/release-profile.md) - also changes the profile `[profile.profiling]` inherits; land one, then rebase the other
