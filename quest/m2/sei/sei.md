# [M] SEI catalog section

## Goal

Hang defines a top-level `sei` section that relates raw H.264 and H.265 SEI NAL
units to an exact video access unit without putting `kind: "sei"` on a media
rendition. The contract is sufficient for byte-faithful stitching and for
applications that consume the sidecar directly.

## Plan

Specify one sidecar track per video rendition, or an equally unambiguous
mapping, and key each sample by video group sequence plus frame ordinal. Do not
require timestamp equality across tracks. Preserve codec, prefix or suffix
placement, original NAL bytes, and order when several SEI units accompany one
access unit.

Represent whether an access unit has sidecar SEI, so a consumer can distinguish
"none exists" from "not available." Missing SEI is valid. Do not define publish
order as delivery: the tracks use independent streams and can arrive in either
order. The separate [delivery quest](/quest/m3/sei-delivery.md) must settle a
cross-track mechanism that never extends the video's existing release deadline;
until then, consumers may expose the sidecar directly but importers retain
in-band SEI.

Version the schema so new semantic views can be added without rewriting the raw
contract. Include fixtures for H.264 and H.265 prefix and suffix SEI, multiple
NAL units, frames with no SEI, group boundaries, a skipped video frame, sidecar
loss, and late arrival.
