# [L] Rust SEI split and stitch

## Goal

Rust MPEG-TS and elementary-stream importers can strip H.264/H.265 SEI into the
Hang `sei` sidecar, while Rust decoders and exporters can stitch already
available sidecars back into the correct access unit without delaying video.
Separation remains opt-in until every supported consumer is ready.

## Plan

Implement one codec-aware split/stitch primitive in `moq-mux` and reuse it from
container gateways and FFI-facing playback rather than creating gateway-local
copies. Implement the proven nonblocking delivery contract rather than assuming
one track arrives first. The stitcher consumes by group sequence and frame
ordinal and restores prefix/suffix placement and ordering.

Round-trip exact NAL bytes through Annex B, length-prefixed samples, and
MPEG-TS. Exercise loss, late sidecar delivery, reconnect at a group boundary,
video-only subscription, and bounded cleanup of unmatched sidecars. A live
test must prove delayed sidecar delivery cannot stall the video track.

## Required

- [H.265 suffix SEI](/quest/m0/h265-suffix.md) - fixes access-unit ownership
  before the splitter makes that ownership a sidecar contract
- [SEI section](/quest/m2/sei/sei.md) - defines the catalog and nonblocking
  correlation contract
- [Versioned SEI profile](/quest/m2/sei/sei-profile.md) - supplies the
  resolver boundary and compatibility fixtures this implementation follows
- [Nonblocking SEI delivery](/quest/m3/sei-delivery.md) - proves the
  cross-track mechanism the implementation must use
