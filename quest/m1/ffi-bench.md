# [L] Binding benchmark harness

## Goal

Python, Kotlin (JVM), and Go each run a benchmark of their wrapper's
publish and subscribe path every night, beside two Rust baselines measured
on the same runner, so FFI overhead is tracked and optimizations are
measured instead of guessed. The nightly job summary shows each binding's
numbers and its ratio to the baselines. This is the shared harness later
binding performance quests measure on.

Swift, Dart, and C++ join in [their own quest](/quest/m1/ffi-bench-wrappers.md).
The trend website is [the site quest](/quest/m1/bench-site.md).

## Plan

Decided with the maintainer on 2026-10-15:

- **Split from [Payload bytes](/quest/m1/ffi-bytes.md)**, which planned this
  benchmark but is blocked behind the UniFFI fork. The harness needs no
  fork, so it starts now and the fork's patches are measured on it.
- **Shape**: an in-process publisher and subscriber, no network
  (`MoqTrackProducer::consume` connects them without a session).
  `write_frame` and `read_frame` throughput over frame sizes from 100 B to
  1 MB, a no-op call, and a record round trip. Sweep publisher and
  subscriber counts independently, as the Go bench it replaces does, so a
  cost that grows with either axis shows as a slope.
- **Metrics**: throughput and per-call time, plus process CPU (user + sys)
  per MB. Async bridging (asyncio, coroutines, goroutines) burns CPU on
  other threads that wall time hides.
- **Layer**: each language benches its idiomatic wrapper (`py/moq-rs`,
  `kt/moq`, `go/wrapper`), which is what users feel. The no-op call goes
  through the raw generated binding to isolate the generator.
- **Two Rust baselines**: the same loop against moq-ffi's Rust API called
  directly (binding / moq-ffi is the boundary's cost) and against moq-net
  (moq-ffi / moq-net is the `ffi::Task` and mutex machinery that
  [the poll spike](/quest/m1/ffi-poll.md) targets). Criterion, like the
  other Rust benches.
- **Tracking**: report ratios to the same-job Rust baselines, which cancel
  most hosted-runner noise. No thresholds. Emit every result in one JSON
  shape (github-action-benchmark's custom format is a good fit) as a nightly
  artifact; [the site](/quest/m1/bench-site.md) publishes the history.
- **Go**: replace the loopback-QUIC `BenchmarkPublisher` in
  `go/wrapper/runtime_bench_test.go` with the in-process harness; the
  nightly "Go publisher runtime benchmark" slot runs the new one. Delete the
  QUIC version.
- Use each language's idiomatic bench tooling where it gives sound
  statistics (Go `testing.B`, Criterion), but avoid heavy new build
  plumbing just for a framework. A plain timed loop is fine.
- Local recipes per language plus one that runs them all, documented in
  `bench/README.md`.

Public API: none. Wire: none.

## Related

- [Payload bytes](/quest/m1/ffi-bytes.md) - the first optimization measured on this harness
- [Benchmark regressions in CI](/quest/m2/bench-ci.md) - the Criterion side of benchmark tracking
