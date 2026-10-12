# [M] Compare Rust-backed @moq/net in media workloads

## Goal

A reproducible browser report compares Rust-backed `@moq/net` with its
retained handwritten baseline through full publish/watch media workloads,
including bundle size, CPU, and first-frame latency.

## Plan

Decided 2026-10-12: this broader media report remains in m2. It supplements
the focused browser comparison required before each backend adoption; it
cannot replace or postpone that decision evidence. Measure whichever backend
is deployed, WASM or JS-only, and name the exact revisions and build inputs.

Follow the browser harness's controls for variability, separate network,
decode, and render costs, and record unsupported metrics explicitly. Keep
baseline artifacts even after deleting handwritten code. Report regressions
and accepted tradeoffs without assuming that generated code is faster.

Public API: none. Wire: none.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - the full media workload harness and artifact conventions
- [Rust-backed @moq/net](/quest/m1/rs2ts/README.md) - the adopted implementation and retained baseline
