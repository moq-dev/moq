# [S] H.264 import publishes warmup from the recovery-point SEI

## Goal

An H.264 stream encoded with intra refresh (x264 `--intra-refresh`, NVENC, or
a broadcast contribution over RTMP, SRT, TS, MKV, or fMP4) imports with one
group per refresh sweep and a catalog `warmup` that tells viewers how long
recovery takes. Today the splitter flags the recovery point as a keyframe but
drops `recovery_frame_cnt`, so a gradual recovery point is published as if it
were immediately decodable.

## Plan

- `rs/moq-mux/src/codec/h264/split.rs`: `sei_has_recovery_point` returns the
  `recovery_frame_cnt` the `h264-parser` crate already exposes on
  `SeiPayload::RecoveryPoint`, and the split frame carries it. A count of zero
  stays a plain keyframe. Recovery points with `broken_link_flag` set remain
  out of scope, as before.
- The length-prefixed path bypasses the splitter: `rs/moq-mux/src/import/track.rs`
  calls `h264::avc1_frame` for fMP4, MKV, and other `avc1` sources, and that
  helper recognises only IDR slices. Move the recovery-point SEI check into a
  helper both paths share, so a recovery point opens a group and carries its
  count whatever the container.
- `rs/moq-mux/src/codec/h264/import.rs`: never estimate `warmup` from a
  framerate. `recovery_frame_cnt` is a `frame_num` distance (H.264 D.2.8: the
  reference picture whose `frame_num` is the recovery point's plus the count,
  modulo `MaxFrameNum`), so the importer tracks slice-header `frame_num` from
  the recovery point, finds that picture, and sets `warmup` to its timestamp
  minus the group start, rounded down to whole milliseconds so the recovery
  picture itself is never withheld. The catalog is published once the first
  sweep has measured it; a later, longer sweep mutates it upward. A stream
  whose recovery picture never arrives before the next recovery point is
  refused, not published with a guess.
- Fixture: a synthetic fixed-framerate access-unit sequence with
  `recovery_frame_cnt = 2` beside the existing recovery-point tests in
  `split.rs`, asserting the exact `warmup`, in both the Annex B and the
  length-prefixed path; and a real clip generated with x264
  `intra-refresh=1` added as a second round-trip in `test/ts/run.sh` next to
  the closed-GOP one, asserting the catalog `warmup` and the group count.

## Required

- [Catalog warmup](/quest/next/catalog-warmup.md) - the field import writes
