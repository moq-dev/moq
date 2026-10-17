# [XS] Pin local benchmark comparisons to one core type

## Goal

On a hybrid CPU (performance and efficiency cores), `bench/run.sh` runs
both sides of a comparison on the same core type, so a base/head delta no
longer depends on where the scheduler put each run. On a CPU with one core
type nothing changes.

## Plan

Decided 2026-10-16 with the maintainer. Seen while landing the moq-quic
benchmarks: on an 8P/16E host the same unpinned case measured 420 µs or
690 µs depending on the core it ran on. Detect the performance cores where
the kernel reports them (for example `/sys/devices/cpu_core/cpus` on Intel
hybrid parts) and pin each benchmark process to them with `taskset`. Print
which cores were used beside the comparison, so a reader knows the run was
pinned.

Public API: none. Wire: none.
