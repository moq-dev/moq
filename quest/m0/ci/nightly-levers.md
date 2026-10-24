# [M] Measure nightly compiler levers in the queue

## Goal

A report says whether building the queue's tests on a pinned nightly with the
parallel frontend and the Cranelift backend saves enough wall time to justify
its risks, measured after the structural queue changes land. If it does, the
queue adopts it, and Nightly keeps a stable LLVM build of the same tests as the
backstop.

## Plan

Decided with the maintainer on 2026-10-23: a measured experiment, last.
Estimated 2026-10-23 at 25 to 35% of the test build's compile, about 10% of
total queue time. Risks to measure, not assume:

- Cranelift's unwinding against moq's 30 `#[should_panic]` tests and 16
  `catch_unwind` files.
- sonora's SIMD echo canceller, forced to opt-level 3 because its realtime
  tests time out unoptimized; it may need a per-package LLVM backend.
- Intrinsics in `memchr`, `simd-adler32`, `crc32fast`, and `blake3`.
- The parallel frontend's remaining incremental races and differing cycle
  errors.
- The gate no longer compiles with the toolchain that ships, and nightly mbx
  keys split from Nightly's stable R2 writers.

kixelated/rust2's [build-speed benchmark](https://github.com/kixelated/rust2/blob/main/quest/m1/bench.md)
measures these levers on moq; use its numbers, and its explanation of nightly
incremental rebuilds costing near-full builds, before running the queue
experiment.

Public API: none. Wire: none.

## Required

- [Build queue entries concurrently](/quest/m0/ci/queue-concurrency.md) - the structural baseline
- [Run the queue's modules in parallel](/quest/m0/ci/parallel-modules.md) - the structural baseline
- [One nextest build in the queue](/quest/m0/ci/one-test-build.md) - the build this would speed up
