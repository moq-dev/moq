# [S] moq export ts: video reorder bound follows the stream

## Goal

Two `moq export ts` legs of one broadcast interleave video and audio
identically even when a B-frame arrives late. Today the media-time interleave
from [#4001](https://github.com/moq-dev/moq/pull/4001) treats a video track as
advanced past a candidate once its high-water mark, less its DTS reserve,
passes it. Without catalog `jitter` that reserve is the 16-tick
`DEFAULT_DTS_RESERVE`, so a B-frame presenting below the mark can still land
after audio it should precede, and the order depends on arrival again. A
`jitter` that shows up in a later catalog is ignored too: `update_catalog`
returns early once the PAT/PMT is built, before the per-track refresh.

## Plan

Decided:

- Keep refreshing a video track's reserve from catalog `jitter` after the PMT
  is emitted. The early return exists to lock the track layout, not the
  per-track timing, and the importer often fills `jitter` only once it has
  seen reordering.
- When `jitter` is absent, grow the reserve from the reordering actually
  observed (how far a frame's PTS falls below the track's high-water mark).
  It only grows, so a stream that never reorders keeps today's tiny reserve.
- Refusing to export video without `jitter` was rejected as too extreme.
- Treating unknown jitter as unbounded was tried in #4001 and rejected: the
  stall never clears when video leads, breaking
  `quiet_track_is_emitted_around_then_rejoins`.

Guidance:

- The same reserve feeds `author_dts`, so growing it mid-stream shifts DTS
  further behind PTS. The existing monotonic clamp keeps DTS from stepping
  back; check the PCR, which backs off by the largest reserve, stays ahead of
  every DTS written.
- A B-frame that lands before the reserve has grown to cover it can still
  reorder output once. Say in a comment that the observed bound converges
  after the first deep reorder, and that catalog `jitter` avoids even that.
- Tests in `export_test.rs`: two exporters with a late B-frame and no
  `jitter` produce byte-identical output once reordering has been seen, and a
  `jitter` arriving in a catalog after the PMT raises the reserve.

## Related

- [TS export byte schedule](/quest/m1/ts-export-byte-schedule.md) - also reshapes PCR placement in `export.rs`
