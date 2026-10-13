# [M] Browser backend baseline

## Goal

A reproducible browser harness compares handwritten js/net and the current
Rust WASM consumer, retains their baselines, and can exercise the complete
bindings before adoption. Measurements and parity fixtures run in CI, at
least nightly, and the report identifies gaps rather than declaring the
current consume-only WASM package a complete replacement.

## Plan

Decided 2026-10-12: move the focused backend comparison before adoption.
Reuse `test/wasm` and existing benchmark conventions; the broader
[media benchmark suite](/quest/m2/browser-benchmarks.md) stays in m2 and
is not a prerequisite. Preserve fixtures independently of backend packaging.

- Record source revisions, Rust/bindgen/Binaryen versions where applicable,
  build features, browser versions, hardware, and reproducible commands.
  Inspect and reproduce the applicable async cases attached to
  [#5337](https://github.com/moq-dev/moq/issues/5337); label unverified
  contributor results separately. Do not equate its publisher prototype
  with the original async consumer or with handwritten js/net.
- Compare complete shipped assets, compressed JS plus WASM and glue, cold
  startup and first-frame latency, per-frame CPU, and memory/copy/GC costs
  where measurable. Use matching workloads and alternating repeated runs;
  report unavailable metrics and measurement variability explicitly.
- Exercise real WebTransport and the supported fallback. Separate protocol,
  boundary, and network costs, and cover small audio through large frames.
  Sweep publisher and subscriber counts so fan-out costs remain visible.
- Retain parity cases for payloads and metadata, negotiated versions,
  cancellation, close/error propagation, backpressure, memory growth, and
  exact u64 values above 2^53 through u64::MAX where legal. Refuse values
  outside each wire version's range; do not round them through JS numbers.
- Deterministic unit tests use controlled time. Browser integration tests
  synchronize on events. Timing reports should not fail a shared runner for
  arbitrary slowdowns; correctness and invalid benchmark samples do fail.

The binding and adoption quests extend this harness to publishing and the
full API. The report is a baseline, not approval to switch implementations.

Public API: none. Wire: none.
