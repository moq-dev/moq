# [S] Profile lock wait off-CPU

## Goal

A `just` recipe runs a bench shape under bpftrace and reports, per relay
worker thread (`moq-uring-N`, `moq-quic-N`), the time spent blocked in futex
waits and the user stacks that blocked, as a share of that thread's CPU. Lock
wait becomes a number anyone can reproduce without code in `kio`.

## Plan

Decided (maintainer, 2026-10-07), after closing #5031, which timed every
contended `kio::Lock` acquire in code and added public API in three crates
plus five Prometheus families:

- Measure from outside the process: bpftrace on the futex syscalls of the
  relay's worker threads, aggregated by thread name and user stack. No
  instrumentation in `kio`, no new public API, no metrics.
- Build the relay with `[profile.profiling]` and
  `RUSTFLAGS="-C force-frame-pointers=yes"`: the release build `bench/run.sh`
  uses has neither debug info nor frame pointers, so `ustack` would stop at
  the libc futex frame. `Consumer::poll` is generic and inlined, and
  bpftrace doesn't expand inline frames, so the stacks name its callers.
- Count only stacks under the std mutex's contended path (`kio::Lock` is a
  `std::sync::Mutex`): futex waits also include allocator, condvar, and tokio
  waits, and a contended acquire that resolves while spinning never reaches
  the futex.
- Add bpftrace to the Nix dev shell for Linux only
  (`lib.optionals stdenv.isLinux`), since the shell also builds for
  `aarch64-darwin`. The recipe needs sudo (or `CAP_BPF`) and says so.
- Local only, documented next to the existing bench recipes (`just bench`,
  `just bench-runtime`). Not in CI, since it needs privileges.
- Shapes and worker counts follow #5031: fanout (`bench/workloads/fanout.toml`)
  and chat (`rs/moq-bench/config/chat.toml`) at 1, 4, and 16 workers, with a
  steady window after startup. Wait is off-CPU, so a thread's share can
  exceed 1 when it blocks longer than it runs.

Check that it roughly reproduces #5031's wait time (see
[kio channel contention](/quest/m1/perf/kio-channel-contention.md)), not its
contended-acquire counts, and that its stacks lead to the same call sites.

## Related

- [kio channel contention](/quest/m1/perf/kio-channel-contention.md) - requires this for its before and after
