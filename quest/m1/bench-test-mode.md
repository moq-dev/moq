# [S] moq-net's benches run to completion

## Goal

Every moq-net bench target runs to completion, both in Criterion's test mode
(`cargo test -p moq-net --features fuzz --benches`) and in Nightly's measured
run, so a bench broken by a behaviour change fails where someone sees it.
Done when that command passes on main, Nightly's "Rust bench targets" job gets
past its `criterion net` step, and the PR records where CI catches the next
such break.

## Plan

The implementation PR fixes the benchmark fixtures without changing the product:

- `origin/reprice_duplicate` uses covering discovery, since the announced prefix
  is above each cursor's allowed paths.
- Memory and session delivery retain unread groups without byte-pressure
  shedding. Cache expiry between rounds bounds history outside measured work.
  The shared session fixture also covers the production relay benchmark.
- The full test-mode and short measured sweeps verify every moq-net target.

Nightly already runs the measured targets in `sh/gh/nightly.sh`'s `rust-bench`
step. The PR gate builds benches with their required features, but excludes
running them through `.config/nextest.toml`'s `not kind(bench)` default filter.
No CI placement change is included pending the maintainer's decision below.

Public API: none. Wire: none.

## Waiting

- Maintainer: choose where CI catches the next benchmark failure. Options:
  retain measured Nightly coverage, or run moq-net benches in test mode in the
  PR gate as well. Recommend retaining Nightly coverage, consistent with the
  Nightly epic's existing policy; the PR records the full test-mode cost.
  This reverses a recorded maintainer policy if changed, so an agent cannot
  choose it unattended. Check the decision is recorded in the PR, then apply
  any chosen gate change and finish this quest.

## Related

- [Nightly passes on main](/quest/m0/nightly/README.md) - this quest owns the moq-net bench panics in Nightly's "Rust bench targets" job
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - calibrates the measurements; this quest only keeps the bench executables correct
