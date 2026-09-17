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
  payload-type and payload-size bytes by hand the way the H.264 helper does:
  skip the two-byte NAL header, strip emulation-prevention bytes first, bound
  the walk to the RBSP, and reject a truncated `se(v)` rather than emitting a
  count. Flag the access unit a keyframe, re-inject cached
  VPS/SPS/PPS ahead of its first slice as the H.264 splitter does for a bare
  recovery point, and carry the count; `broken_link_flag` stays out of scope.
- The length-prefixed path calls `h265::hvc1_frame` directly and bypasses the
  splitter, as for H.264; share the SEI helper so `hvc1` sources form groups
  too.
- `rs/moq-mux/src/codec/h265/import.rs`: `recovery_poc_cnt` is a picture
  order count distance, the same displayed-picture semantics as H.264's
  `frame_num` distance, so the importer finds the picture whose POC is the
  recovery point's plus the count and measures `warmup` from its timestamp
  exactly as the H.264 quest does, sharing the measurement and catalog
  mutation.
- Fixture: synthetic NAL sequences beside the existing suffix-SEI tests, since
  x265 cannot produce intra refresh; a real NVENC HEVC clip is verified by hand
  when the NVENC quest lands.

## Required

- [Catalog warmup](/quest/m2/intra-refresh/catalog-warmup.md) - the field import writes

## Related

- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - the conversion and catalog mutation this shares
