# [S] TS export leaves a backlog in its sources

## Goal

`moq export ts` reads a track only until it has a frame due, so a backlog (a
stalled sink, a large retained group) stays in the source and goes out as it
falls due instead of being queued whole in the jitter buffer first. Output
order, deadlines, and late drops are unchanged. A regression test on mock time
shows the bound, at zero and nonzero delay.

## Plan

[#5309](https://github.com/moq-dev/moq/pull/5309) bounded FLV and MKV this way
with the jitter buffer's `full` check: a track is full once a frame of it is
due, or holds any frame without a delay. TS `fill` still drains every ready
frame, as it did before that PR. Reuse the same check rather than a TS-specific
bound.

Wait for the TS work in flight to land first, so this does not fight it:
[#5349](https://github.com/moq-dev/moq/pull/5349) (passthrough export) still
reshapes the export, and [#5305](https://github.com/moq-dev/moq/pull/5305)
(stitch catalog bound) has landed.

Watch for TS-only interactions: the zero-delay path lays grid slots from each
released frame, the program tables hold one frame per track before they are
built, and tune-in alignment drops audio ahead of the first video. Do not cap
how far ahead of now a track is read: a track sent ahead of the others (TS video
often leads audio) would be read late, skewing the leads and drift the clock
measures. Run the TS export benchmarks to show the cost.

## Required

- [TS passthrough export](/quest/m1/ts-passthrough-export.md) - `export ts --passthrough` lands, settling the export's read path
