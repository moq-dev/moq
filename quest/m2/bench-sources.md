# [M] Benchmark sources

## Goal

Every nightly benchmark job that measures something feeds the
[benchmark regression table](https://git.moq.dev/bench/), and no series on it
is a smoke run that measures nothing.

## Plan

Split from the benchmark trend site on 2026-10-20, when that quest became
the m0 table. The Criterion targets, the JS benchmarks, the Rust benchmark
tests and the nightly shipping sizes feed it now
([moq-dev/moq#7510](https://github.com/moq-dev/moq/pull/7510) added the last
two). The binding runners stay an unmeasured smoke run: rust2 measures them,
and [moq-dev/moq#6629](https://github.com/moq-dev/moq/pull/6629) retired their
series. What remains joins through the same input format; the sources decided
with the maintainer on 2026-10-15 left these for later:

- The relay load benches, `bench-audio` and `bench-shard` (the "Audio grouping
  relay benchmark" and "Relay shard sweep" Nightly jobs). Each writes a JSON
  summary per workload; pick the metrics a trend should follow (delivered
  throughput, relay CPU, loss, latency) and convert them in
  `sh/gh/bench-report.py`. "Relay profiles" and "Browser benchmark smoke" only
  check their capture paths and stay out.
- The audio-quality job's benchmarks: `just js qmux-bench`, `serve-bench` and
  `ring-bench`. Check each prints a CSV table the `js` converter reads before
  recording it.

A source joins as a `bench-*` artifact with a recorded outcome, which the
benchmark-site job collects. The paired nightly comparison publishes beside the
table from the same `gh-pages` branch (`bench/paired.json`); a new Rust source
joins it through `just gh benchmark-report rust`.

Public API: none. Wire: none.
