# [M] fMP4 emsg carriage

## Goal

`emsg` boxes survive fMP4 import instead of being silently discarded, and export
reproduces them. In-band ID3, SCTE-35 splice information, and DASH event
messages reach a MoQ consumer.

## Plan

`rs/moq-mux/src/container/fmp4/` does not mention `emsg` anywhere, so every
event message in a fragmented MP4 is dropped without a log line. That is the
one metadata path a DASH or CMAF ingest is most likely to use.

Carry the boxes byte-faithfully: scheme id URI, value, timescale, presentation
time, duration, id, and the opaque message payload, for both version 0 and
version 1 timing. Do not parse the payload into a vocabulary; an application
decodes it with its own library, and a new scheme stays forward-compatible.

Carry them on a sidecar of the video rendition (the audio rendition when there
is none) following the rule in [SEI section](/quest/m2/sei/sei.md): the
rendition's group sequence, each box stamped with the wire timestamp of its
presentation time, raw bytes. Export rebuilds the box in the fragment whose
media time contains it.

Test version 0 and version 1 boxes, an emsg before the first moof, several on
one fragment, an unknown scheme, a zero duration, and a round trip that is
byte-identical.

## Related

- [ID3 catalog section](/quest/m2/id3.md) - gives one payload type carried here a
  typed contract
- [AV1 metadata OBUs](/quest/m2/av1-metadata.md) - the same silent drop in a
  different layer
- [FLV script tags](/quest/m2/flv-script.md) - likewise
