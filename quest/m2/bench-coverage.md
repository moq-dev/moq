# [S] Benchmarks for patterns and containers

## Goal

Hot paths that have no benchmark get Criterion targets, so CI tracks them:
`moq-pattern` path matching, and `moq-mux` container import and export per
frame.

## Plan

Deferred to m2 in the 2026-09-30 audit. The `moq-pattern` benchmark is the
one worth doing first, and could return to m1 as its own quest.

Decided 2026-10-08: shrunk to patterns and containers, the paths that run
per announce or per frame. `hang` catalog updates and `moq-auth` token
verification run per update or per connection.

- `moq-pattern`: matching swept over pattern count and path depth. Patterns
  shipped as `rs/moq-pattern` and `js/pattern`, so the matcher gets one bench
  here, not a second one elsewhere.
- `moq-mux`: fMP4/CMAF and Annex-B import and export over checked-in or
  generated fixtures, with throughput in frames and bytes. Keep fixture
  generation out of the timed region. `rs/moq-mux/benches` holds only the
  catalog churn bench today.

Anything that fans out gets a sweep over both axes. Name each target after
what it measures, so the CI comment reads without opening the file.

Public API: none. Wire: none.

## Related

- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - tracks these targets on PRs and nightly
