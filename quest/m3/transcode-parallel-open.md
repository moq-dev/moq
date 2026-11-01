# [XS] A transcode ladder probes an unseen source shape without stalling its other rungs

## Goal

When a moq-transcode source changes to a size the ladder hasn't seen,
resolving the ladder opens and probes the new encoders off
`Transcoder::run`'s `select!` loop and in parallel, so the running
rungs keep encoding and the catalog republishes after the slowest probe
rather than the sum of them.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375),
which finishes [transcode rungs](/quest/m2/transcode-rungs.md). Milestone m3: [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375)'s probe memo
already makes each shape probe once, so this only matters for a source that
keeps changing to new sizes.

The resolve for an unmemoized shape runs serially inside the `select!` in
`Transcoder::run` (`rs/moq-transcode/src/lib.rs:117`, loop at `:170`),
blocking every rung meanwhile. Open the per-rung encoders concurrently (a
`kio::Tasks` or joined futures), and have the loop keep encoding while the
resolve is pending. Only the resolve for the newest source shape may
commit: a late resolve for a superseded shape is dropped (or the pending
one is cancelled), so a source changing A, B, C ends on C even when B
resolves last; test that order. Extend [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375)'s `ladder` bench
(`rs/moq-transcode/benches/ladder.rs`) `flapping` case with unseen shapes
to show the difference.

Public API: none. Wire: none.

## Required

- [Transcode rungs](/quest/m2/transcode-rungs.md) - its PR, [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375), adds the probe memo and ladder bench this builds on
