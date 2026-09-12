# [S] One copy from FLV/RTMP tag body to hang payload

## Goal

FLV import and whole-message RTMP chunks copy coded bytes once, from the
tag/chunk buffer to the hang payload.

## Plan

FLV `Bytes::copy_from_slice` of the tag body, then `write_video` copies
`data` again. RTMP chunks `extend_from_slice` into `current_payload_data`
(needed for split chunks); default chunk size makes this a second copy of
every AVC NALU.

`split_to` the tag from `BytesMut` and slice AVC payload without a second
copy. For whole-message RTMP chunks, freeze the chunk bytes instead of
copying into a new `BytesMut`.

Acceptance: Criterion `flv.import` plus RTMP chunk deserializer on a 1080p
FLV/AVC recording.

## Related

- [FLV script tags](/quest/m2/flv-script.md) - metadata, not media copies
- [Annex-B split](/quest/m2/mux-copies/annexb.md) - NAL copies after the tag
