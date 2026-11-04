# [S] Measure what a catalog republish costs per rendition edit

## Goal

A benchmark measures whole-catalog re-serialization per rendition edit
across track counts, and its numbers decide between catalog deltas, a bound
on catalog size, or leaving it as is, recorded in the PR.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

Every rendition edit re-serializes and republishes the whole catalog, so
the cost grows with the track count.

- Bench serialize plus publish per edit at, for example, 1, 16, 256, and
  4,096 tracks, in Rust and JS.
- Then bring the choice back: deltas (see the JSON diff quest), a documented
  bound on tracks per catalog, or no change.

Public API: none for the measurement. Wire: none.

## Related

- [Benchmark a Value merge-patch diff for moq-json](/quest/m2/json-value-diff.md) - the delta machinery a catalog diff would reuse
