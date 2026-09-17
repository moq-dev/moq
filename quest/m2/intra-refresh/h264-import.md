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
- `rs/moq-mux/src/codec/h264/import.rs`: convert the count to a duration
  using the rendition's framerate when the SPS or container gives one, else
  the observed frame interval, and publish it as `warmup`. Take the largest
  count seen and mutate the catalog when it grows; a stream where neither
  framerate nor an interval is available and the count is nonzero is refused,
  not published with a guess.
- Fixture: a synthetic access-unit sequence with `recovery_frame_cnt = 2`
  beside the existing recovery-point tests in `split.rs`, and a real clip
  generated with x264 `intra-refresh=1` added as a second round-trip in
  `test/ts/run.sh` next to the closed-GOP one, asserting the catalog `warmup`
  and the group count.

## Required

- [Catalog warmup](/quest/m2/intra-refresh/catalog-warmup.md) - the field import writes
