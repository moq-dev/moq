# [M] Compare Rust-backed @moq/net in media workloads

## Goal

A reproducible browser report compares Rust-backed `@moq/net` with its
retained handwritten baseline through full publish/watch media workloads,
including bundle size, CPU, and first-frame latency.

## Plan

Decided 2026-10-12: this broader media report remains in m2. It supplements
the focused browser comparison required before each backend adoption; it
cannot replace or postpone that decision evidence. Measure whichever backend
is deployed and name the exact revisions and build inputs.

Run it as a variant of `bench/browser` (`just bench-browser BASE`), whose A/A
and injected-copy controls bound variability and which already separates
network, decode, and render costs and records unsupported metrics. Keep
baseline artifacts even after deleting handwritten code. Report regressions
and accepted tradeoffs without assuming that generated code is faster.

Public API: none. Wire: none.

## Required

- [JS-only @moq/net](/quest/m2/rs2ts-js.md) - the adopted implementation and retained baseline
