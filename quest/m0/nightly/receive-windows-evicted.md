# [S] The receive-windows benchmark runs to the end

## Goal

`moq-tokio`'s `benchmark_receive_windows`
(`rs/moq-tokio/tests/integration/receive_credit.rs:1056`) passes on `main`
instead of failing with `frame: Evicted`, so Nightly's `Rust benchmark
tests` job (`gh nightly rust-test`) passes and that night's report is
published. Fixed at the cause, never by loosening what the benchmark
measures.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

It failed in 3 of 3 local runs on `main`, and the job's failure skips the
rest of the night's report. Likely since
[moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597) changed its
subscription to a track `Delay { min, max }`: confirm by running the
benchmark at #7597's parent and at #7597.

- Repro: `cargo nextest run -p moq-tokio --release -E
  'test(benchmark_receive_windows)' --run-ignored only`.
- Decide whether the benchmark's subscription now asks for less than it
  reads (a zero `delay.max` evicting groups the reader still wants), or
  whether eviction changed under it, and fix that side.
- [Receive pool](/quest/m2/receive-pool.md) sweeps this harness later; keep
  its cases runnable.

Public API: none. Wire: none.

## Required

- [A group a subscriber gives up on is always reported](/quest/m1/group-drops.md) - overcommitted cells need the subscriber to report groups it abandons, or the paced writer stalls
