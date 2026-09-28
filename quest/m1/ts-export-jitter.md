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
- When `jitter` is absent, derive the reserve from the stream rather than
  refuse it. The bitstream's reorder depth (H.264 VUI
  `max_num_reorder_frames`, HEVC `sps_max_num_reorder_pics`) counts
  pictures, not time, so it becomes a deterministic 90 kHz bound only with
  fixed timing: the VUI `fixed_frame_rate_flag` with its tick, or the
  catalog framerate. With both, the reserve is depth times frame duration,
  known from the first keyframe before any frame is emitted. AV1 and VP9
  present in decode order and need no reserve. Otherwise (variable frame
  rate, or nothing declared), grow the reserve from the reordering actually
  observed (how far a frame's PTS falls below the track's high-water mark);
  it only grows, so a stream that never reorders keeps today's tiny reserve.
- Refusing to export video without `jitter` was rejected as too extreme.
- Treating unknown jitter as unbounded was tried in #4001 and rejected: the
  stall never clears when video leads, breaking
  `quiet_track_is_emitted_around_then_rejoins`.

Guidance:

- The same reserve feeds `author_dts`, so growing it mid-stream shifts DTS
  further behind PTS. The existing monotonic clamp keeps DTS from stepping
  back; check the PCR, which backs off by the largest reserve, stays ahead of
  every DTS written.
- Only the observed fallback is nondeterministic: a B-frame deeper than any
  seen before can reorder output once per new maximum (Codex on #4307).
  Refusing such streams was ruled out by the maintainer. Say
  so in a comment and log each growth, so an undeclared stream is visible
  rather than silently misordered.
- Tests in `export_test.rs`: two exporters with a late B-frame, no `jitter`,
  and a declared reorder depth at a fixed frame rate produce byte-identical
  output from the first frame; an undeclared stream converges after its deepest reorder; and a
  `jitter` arriving in a catalog after the PMT raises the reserve.

## Related

- [TS export byte schedule](/quest/m1/ts-export-byte-schedule.md) - also reshapes PCR placement in `export.rs`
