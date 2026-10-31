# [S] Measure the parallel frontend in the queue

## Goal

A report says whether building the queue's tests on a pinned nightly with the
parallel frontend saves enough wall time to justify its risks, measured after
the structural queue changes land. If it does, the queue adopts it, and
Nightly keeps a stable build of the same tests as the backstop.

## Plan

Decided with the maintainer on 2026-10-23: a measured experiment, last.
Estimated 2026-10-23 at 25 to 35% of the test build's compile, about 10% of
total queue time, with Cranelift included. Risks to measure, not assume:

- The parallel frontend's remaining incremental races and differing cycle
  errors.
- The gate no longer compiles with the toolchain that ships, and nightly mbx
  keys split from Nightly's stable R2 writers.

Decided 2026-10-27 (maintainer): moq keeps LLVM, accepting moq-dev/rust2's
Cranelift verdict ([build benchmark](https://github.com/moq-dev/rust2/blob/main/bench/build/README.md),
"Cranelift for moq's CI", 2026-10-25). 42 of moq's 5418 tests fail under
Cranelift (panics it cannot unwind, and an x86 SIMD intrinsic it lacks), and
the CPU saving is only 4% with a warm cache and 12% cold. This quest is
narrowed to the parallel frontend. Use the same benchmark's numbers, and its
explanation of nightly incremental rebuilds costing near-full builds, before
running the queue experiment.

Public API: none. Wire: none.

## Required

- [Land main through quest land](/quest/m0/ci/land.md) - the structural baseline, replacing concurrent queue entries
