# [S] FLV script tag carriage

## Goal

FLV script tags survive RTMP and FLV import instead of being discarded, so
`onMetaData` and application AMF data messages reach a MoQ consumer.

## Plan

The shared metadata contract owns timestamp encoding and placement. Adopt it
before implementation; deferred SEI extraction is not a prerequisite.

`rs/moq-mux/src/container/flv/import.rs` matches `TAG_SCRIPT => {}` and moves
on, which drops every AMF data message an encoder sends. `onMetaData` is the
one every RTMP publisher emits, and applications routinely push their own cues
through the same channel.

Carry raw tag payloads without decoding AMF into a fixed vocabulary. Publish
when received on independently sequenced metadata groups, with event time on
the broadcast clock and source placement per
[Metadata association](/quest/m2/metadata-association.md). Tags before media
are delivered immediately and retain explicit pre-media placement; audio-only
and script-only input do not require a dummy video rendition.
Where `onMetaData` duplicates something the catalog already models (dimensions,
framerate, bitrate), prefer the value the bitstream actually carries and treat
the script tag as opaque data, not a second source of truth for configuration.

Export reproduces the tags on the FLV path. Test `onMetaData`, an application
message with a custom name, AMF0 and AMF3 payloads, a tag before the first
media tag, and a byte-identical round trip.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the required M1 APIs must be available on main before this implementation starts

- [Broadcast clock](/quest/m1/broadcast-clock.md) - event times use the landed shared clock contract

- [Metadata association contract](/quest/m2/metadata-association.md) - settles the shared framing and missing-data semantics before this section adopts them

## Related

- [fMP4 emsg carriage](/quest/m2/emsg.md) - the same silent drop in a different
  layer
- [AV1 metadata OBUs](/quest/m3/av1-metadata.md) - likewise
