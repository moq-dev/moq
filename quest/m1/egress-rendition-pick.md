# [S] Single-rendition egress picks the best rendition

## Goal

An egress that can carry one video rendition serves the highest-quality one
the client can decode, not the first by track name: WHEP (`moq-rtc`),
non-multitrack RTMP play and FLV export. A multi-rendition broadcast serves
the same picture regardless of how its tracks are named.

## Plan

`moq-rtc`'s `pick_track` (`rs/moq-rtc/src/egress.rs`), FLV's `bind_video`
(`rs/moq-mux/src/container/flv/export.rs`), and RTMP's
`check_play_capabilities` (`rs/moq-rtmp/src/server.rs`) each take the first
catalog entry, which is BTreeMap name order. Share one ranking with the
player's fallback and `catalog::choose_source`: largest resolution, then
highest bitrate, among renditions the egress supports. Multitrack FLV/RTMP
and TS/SRT keep carrying every rendition. Test with a catalog whose
lower-quality rendition sorts first.

## Related

- [WHEP ABR](/quest/m2/whep-abr.md) - switch the served rendition per peer instead of fixing one
- [Transcode source](/quest/m1/transcode-source.md) - the same largest-rendition ranking for transcode input
