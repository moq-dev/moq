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
- Add bpftrace to the Nix dev shell. The recipe needs sudo (or `CAP_BPF`)
  and says so.
- Local only, documented next to the existing bench recipes (`just bench`,
  `just bench-runtime`). Not in CI, since it needs privileges.
- Shapes and worker counts follow #5031: fanout (`bench/workloads/fanout.toml`)
  and chat (`rs/moq-bench/config/chat.toml`) at 1, 4, and 16 workers, with a
  steady window after startup. Wait is off-CPU, so the share can exceed 1
  when workers block together.

Check that it reproduces #5031's numbers (see
[kio channel contention](/quest/m1/perf/kio-channel-contention.md)) and
names the same call sites.

## Related

- [kio channel contention](/quest/m1/perf/kio-channel-contention.md) - requires this for its before and after
