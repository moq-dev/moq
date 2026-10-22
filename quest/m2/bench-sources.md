# [S] Benchmark sources

## Goal

Every nightly benchmark job that measures something feeds the
[benchmark regression table](https://git.moq.dev/bench/), and no series on it
is a smoke run that measures nothing.

## Plan

Split from the benchmark trend site on 2026-10-20, when that quest became
the m0 table. Sources decided with the maintainer on 2026-10-15: the binding
benchmark's JSON and the "Rust benchmark tests", "Rust bench targets" and "JS
benchmarks" nightly jobs, converted to the history input format where they
don't already emit one.

The binding benchmark, the Rust bench targets, and the JS benchmarks already
feed the table, measuring
([#5759](https://github.com/moq-dev/moq/pull/5759)). What remains:

- The "Rust benchmark tests" (`rust-test`) job is not recorded yet: its
  nextest benchmarks print no structured measurements.
- Nightly size history (today a 45-day artifact) is a natural extra series
  if it fits cheaply.
- The relay load benches (`bench-audio`, `bench-profile`) and the
  audio-quality job's benchmarks can join later through the same input
  format.
- [Nightly benchmark regression report](/quest/m2/bench-ci.md) records its nightly
  Criterion history on the same `gh-pages` branch instead of creating its own.

Public API: none. Wire: none.
