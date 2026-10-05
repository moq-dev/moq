# [S] FLV script tag carriage

## Goal

FLV script tags survive RTMP and FLV import instead of being discarded, so
`onMetaData` and application AMF data messages reach a MoQ consumer.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer for timed metadata.

`rs/moq-mux/src/container/flv/import.rs` matches `TAG_SCRIPT => {}` and moves
on, which drops every AMF data message an encoder sends. `onMetaData` is the
one every RTMP publisher emits, and applications routinely push their own cues
through the same channel.

Carry raw tag payloads without decoding AMF into a fixed vocabulary. Publish
when received on independently sequenced metadata groups, with event time on
the broadcast clock and source placement per the contract
[emsg](/quest/m3/emsg.md) settles. Tags before media
are delivered immediately and retain explicit pre-media placement; audio-only
and script-only input do not require a dummy video rendition.
Where `onMetaData` duplicates something the catalog already models (dimensions,
framerate, bitrate), prefer the value the bitstream actually carries and treat
the script tag as opaque data, not a second source of truth for configuration.

Export reproduces the tags on the FLV path. Test `onMetaData`, an application
message with a custom name, AMF0 and AMF3 payloads, a tag before the first
media tag, and a byte-identical round trip.

## Required

- [fMP4 emsg](/quest/m3/emsg.md) - settles the shared timed-metadata contract this builds on, which needs maintainer agreement first

## Related

- [AV1 metadata OBUs](/quest/m2/av1-metadata.md) - likewise
