# [M] fMP4 emsg carriage

## Goal

`emsg` boxes survive fMP4 import instead of being silently discarded, and export
reproduces them. In-band ID3, SCTE-35 splice information, and DASH event
messages reach a MoQ consumer.

## Plan

The shared metadata contract owns timestamp encoding and placement. Adopt it
before implementation; deferred SEI extraction is not a prerequisite.

`rs/moq-mux/src/container/fmp4/` does not mention `emsg` anywhere, so every
event message in a fragmented MP4 is dropped without a log line. That is the
one metadata path a DASH or CMAF ingest is most likely to use.

Carry the boxes byte-faithfully: scheme id URI, value, timescale, presentation
time, duration, id, and the opaque message payload, for both version 0 and
version 1 timing. Do not parse the payload into a vocabulary; an application
decodes it with its own library, and a new scheme stays forward-compatible.

Publish each box when received on independently sequenced metadata groups,
using [Metadata association](/quest/next/metadata-association.md). Keep event
presentation time separate from the containing fragment or pre-media
placement. A future event is delivered before its presentation time, and a
box before the first moof does not wait for a media group to exist. Preserve
source placement for exact export rather than selecting a fragment solely
from the event's presentation timestamp.

Test version 0 and version 1 boxes, an emsg before the first moof, several on
one fragment, an unknown scheme, a zero duration, and a round trip that is
byte-identical.

## Required

- [Merge dev](/quest/dev/merge-dev.md) - the required dev APIs must be available on main before this implementation starts

- [Metadata association contract](/quest/next/metadata-association.md) - settles the shared framing and missing-data semantics before this section adopts them

## Related

- [ID3 catalog section](/quest/next/id3.md) - gives one payload type carried here a
  typed contract
- [AV1 metadata OBUs](/quest/future/av1-metadata.md) - the same silent drop in a
  different layer
- [FLV script tags](/quest/next/flv-script.md) - likewise
