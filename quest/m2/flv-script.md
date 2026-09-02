# [S] FLV script tag carriage

## Goal

FLV script tags survive RTMP and FLV import instead of being discarded, so
`onMetaData` and application AMF data messages reach a MoQ consumer.

## Plan

`rs/moq-mux/src/container/flv/import.rs` matches `TAG_SCRIPT => {}` and moves
on, which drops every AMF data message an encoder sends. `onMetaData` is the
one every RTMP publisher emits, and applications routinely push their own cues
through the same channel.

Carry the tag payload byte-faithfully with its timestamp rather than decoding
AMF into a fixed vocabulary, so a custom message type needs no further work.
Where `onMetaData` duplicates something the catalog already models (dimensions,
framerate, bitrate), prefer the value the bitstream actually carries and treat
the script tag as opaque data, not a second source of truth for configuration.

Export reproduces the tags on the FLV path. Test `onMetaData`, an application
message with a custom name, AMF0 and AMF3 payloads, a tag before the first
media tag, and a byte-identical round trip.

## Related

- [fMP4 emsg carriage](/quest/m2/emsg.md) - the same silent drop in a different
  layer
- [AV1 metadata OBUs](/quest/m2/av1-metadata.md) - likewise
