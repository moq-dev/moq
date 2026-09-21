# [L] fMP4 emsg carriage, and the timed-metadata contract it sets

## Goal

`emsg` boxes survive fMP4 import instead of being silently discarded, and export
reproduces them. In-band ID3, SCTE-35 splice information, and DASH event
messages reach a MoQ consumer. This is the first of the four timed-metadata
carriers, so it also settles the contract they share: how an event's
presentation time and its delivery time are framed, how a metadata group
sequences independently of media, how source placement is kept for exact
export, and what missing data means; ID3, SCTE-35, and FLV script tags adopt
it rather than restating it. Deferred SEI extraction is not a prerequisite.

## Plan

The shared contract, recorded here and mirrored in the Hang draft:

- Publish an event as soon as it is received, on its own group sequence; never
  wait for a media GOP or require a media rendition to exist, so advance ad
  cues, sparse metadata, audio-only, and metadata-only sources all work.
- Carry the event's presentation time on the broadcast clock, distinct from
  the outer frame timestamp; a future cue precedes a current event, so test
  retention and expiry for that order, since relay expiry reads group
  timestamps.
- Keep source placement (before the first media, container position, order
  among equal timestamps) separate from event time, and state what export
  reconstructs exactly and what is only equivalent.
- Missing data is reported, not inferred: an export deadline shares the mux
  budget and says metadata was unavailable rather than claiming the source
  had none, and a sparse track never waits indefinitely.

A new exported envelope shape needs maintainer agreement before the
dependents implement against it; land the shared CI fixtures with it.

`rs/moq-mux/src/container/fmp4/` does not mention `emsg` anywhere, so every
event message in a fragmented MP4 is dropped without a log line. That is the
one metadata path a DASH or CMAF ingest is most likely to use.

Carry the boxes byte-faithfully: scheme id URI, value, timescale, presentation
time, duration, id, and the opaque message payload, for both version 0 and
version 1 timing. Do not parse the payload into a vocabulary; an application
decodes it with its own library, and a new scheme stays forward-compatible.

Publish each box when received on independently sequenced metadata groups
under the contract above. Keep event
presentation time separate from the containing fragment or pre-media
placement. A future event is delivered before its presentation time, and a
box before the first moof does not wait for a media group to exist. Preserve
source placement for exact export rather than selecting a fragment solely
from the event's presentation timestamp.

Test version 0 and version 1 boxes, an emsg before the first moof, several on
one fragment, an unknown scheme, a zero duration, and a round trip that is
byte-identical.

## Related

- [ID3 catalog section](/quest/next/id3.md) - gives one payload type carried here a
  typed contract
- [AV1 metadata OBUs](/quest/future/av1-metadata.md) - the same silent drop in a
  different layer
- [FLV script tags](/quest/next/flv-script.md) - likewise
