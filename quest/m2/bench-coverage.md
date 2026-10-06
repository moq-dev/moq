# [M] Benchmarks for patterns, containers, catalog, and auth

## Goal

Hot paths that have no benchmark get Criterion targets, so CI tracks them:
`moq-pattern` path matching, `moq-mux` container import and export per frame, `hang` catalog encode, decode, and
update, and `moq-auth` token verification.

## Plan

Deferred to m2 in the 2026-09-30 audit. The `moq-pattern` benchmark is the
one worth doing first, and could return to m1 as its own quest. The
`moq-stats` producer benchmark split out into
[its own quest](/quest/m2/stats-producer-bench.md) in the 2026-10-06 audit,
since [Binary delta stats](/quest/m2/stats-delta.md) gates on it alone.

- `moq-pattern`: matching swept over pattern count and path depth. Patterns
  shipped as `rs/moq-pattern` and `js/pattern`, so the matcher gets one bench
  here, not a second one elsewhere.
- `moq-mux`: fMP4/CMAF and Annex-B import and export over checked-in or
  generated fixtures, with throughput in frames and bytes. Keep fixture
  generation out of the timed region.
- `hang`: catalog encode and decode swept over rendition count, plus the
  per-update cost a publisher pays.
- `moq-auth`: JWT verification per connection, swept over algorithm and claim
  size. The in-band token path gets its bench with
  [In-band token](/quest/m1/auth/token-in-band.md), not here.

Anything that fans out gets a sweep over both axes. Name each target after
what it measures, so the CI comment reads without opening the file.

Public API: none. Wire: none.

## Related

- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - tracks these targets on PRs and nightly
- [Benchmark the moq-stats producer](/quest/m2/stats-producer-bench.md) - the stats producer target, split out of this quest
