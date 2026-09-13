# [S] AV1 metadata OBUs

## Goal

Keep metadata OBUs inline while evaluating whether separate delivery is worth
its cost, following the same decision as H.26x SEI. The implementation below
is conditional on a positive verdict and renewed scope.

`OBU_METADATA` survives AV1 import and export instead of riding along
unexamined. HDR10+ dynamic metadata, timecode, and scalability structure reach a
consumer that asks for them.

## Plan

`rs/moq-mux/src/codec/av1/split.rs` parses the OBU header to find frame
boundaries and passes everything else through inside the frame, so metadata OBUs
are neither separated nor addressable. This is the AV1 analogue of SEI in
H.26x, and it should follow whatever the `sei` section settles rather than
inventing a second shape.

It gets its own section, like every timed-metadata format, but follows the
sidecar rule the `sei` section defines: one track per AV1 rendition, the
rendition's group sequence, the temporal unit's wire timestamp on each frame,
raw OBU bytes. The payloads overlap with SEI (both carry ITU-T T.35 user data
and timecode), so the typed helpers can be shared even though the framing is
not.

Preserve `metadata_type`, the payload bytes, and ordering within the temporal
unit. Test HDR10+ T.35 metadata, timecode, an unknown metadata type, several
OBUs in one temporal unit, and a byte-identical round trip.

## Required

- [Metadata association contract](/quest/m3/sei/sei.md) - settles the shared framing and missing-data semantics before this section adopts them

## Related

- [SEI sidecars](/quest/m3/sei/README.md) - the H.26x contract this should
  follow rather than duplicate
- [fMP4 emsg carriage](/quest/m2/emsg.md) - independent carriage of container metadata, not a prerequisite for codec extraction
