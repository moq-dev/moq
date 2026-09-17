# [S] H.265 import reads the recovery-point SEI

## Goal

An HEVC stream encoded with intra refresh imports with one group per sweep and
a catalog `warmup`, the same as H.264. Today the H.265 splitter only treats
IRAP NAL types as keyframes and ignores prefix SEIs, so such a stream produces
a track with no keyframes at all and fails the group invariant.

## Plan

- `rs/moq-mux/src/codec/h265/split.rs`: parse `PrefixSeiNut` payloads for
  payload type 6 (`recovery_point`: `recovery_poc_cnt`, `exact_match_flag`,
  `broken_link_flag`). `scuffle-h265` does not parse SEI, so walk the
  payload-type and payload-size bytes by hand the way the H.264 helper does and
  read the one `se(v)`. Flag the access unit a keyframe, re-inject cached
  VPS/SPS/PPS ahead of its first slice as the H.264 splitter does for a bare
  recovery point, and carry the count; `broken_link_flag` stays out of scope.
- `rs/moq-mux/src/codec/h265/import.rs`: publish `warmup` from the count the
  same way as H.264, sharing the conversion.
- Fixture: synthetic NAL sequences beside the existing suffix-SEI tests, since
  x265 cannot produce intra refresh; a real NVENC HEVC clip is verified by hand
  when the NVENC quest lands.

## Required

- [Catalog warmup](/quest/m2/intra-refresh/catalog-warmup.md) - the field import writes

## Related

- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - the conversion and catalog mutation this shares
