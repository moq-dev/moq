# [M] Scan Annex-B in place on complete access units

## Goal

The complete-AU path copies coded bytes at most once per AU. RTC ingest
CPU drops without changing published Annex-B.

## Plan

`NalIterator` `copy_to_bytes` per NAL; `Split` copies each NAL into a new
AU (`extend_from_slice` start code + nal). `Avc1::transform` clones the AU,
splits, then length-prefixes. RTC H.264 `Bridge::push` runs Split on AUs
str0m already reassembled.

Scan in place; emit one AU `Bytes` slice when the AU is already contiguous.
RTC/FLV/TS skip Split when the caller already has a complete AU (only
inject missing SPS/PPS on keyframes). Length-prefix by rewriting prefixes
in one buffer when 4-byte start codes already match.

Acceptance: Criterion `h264.split` / `h264.avc1_transform` on a 1080p
IDR+P GOP; RTC ingest microbench of `Bridge::push`. ≤1 copy of coded bytes
per AU on the complete-AU path.

## Related

- [MPEG-TS ingest](/quest/m2/mux-copies/ts.md) - TS still runs Split today
- [FLV and RTMP tag bodies](/quest/m2/mux-copies/flv-rtmp.md) - FLV also
