# [M] Benchmarks for containers, catalog, auth, and path matching

## Goal

Hot paths that have no benchmark get Criterion targets, so CI tracks them:
`moq-mux` container import and export per frame, `hang` catalog encode, decode,
and update, `moq-auth` token verification, and `moq-pattern` path matching.

## Plan

- `moq-mux`: fMP4/CMAF and Annex-B import and export over checked-in or
  generated fixtures, with throughput in frames and bytes. Keep fixture
  generation out of the timed region.
- `hang`: catalog encode and decode swept over rendition count, plus the
  per-update cost a publisher pays.
- `moq-auth`: JWT verification per connection, swept over algorithm and claim
  size. The in-band token path gets its bench with
  [In-band token](/quest/m1/auth/token-in-band.md), not here.
- `moq-pattern`: matching swept over pattern count and path depth.

Anything that fans out gets a sweep over both axes. Name each target after
what it measures, so the CI comment reads without opening the file.

## Related

- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - tracks these targets on PRs and nightly
