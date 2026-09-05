# [M] Rust SEI split and reinsert

## Goal

Rust importers strip H.264/H.265 SEI out of the access unit into the Hang `sei`
sidecar, and Rust exporters put it back byte-faithfully. A round trip through
any supported container reproduces the original bitstream.

## Plan

Implement one codec-aware split and reinsert primitive in `moq-mux` and reuse it
from every container gateway rather than creating gateway-local copies.
Stripping is the default, not an opt-in: the bytes end up in the sidecar or in
the video, never both.

The exporter joins by group sequence and frame ordinal and restores prefix and
suffix placement and ordering. Placement is exact or it is a loss; there is no
useful approximate reinsertion.

Export is a live consumer of two independently scheduled tracks, so state its
join budget rather than assuming the sidecar is already there. Reinsert within
the mux buffer the exporter already holds for its audio and video interleave,
and past that budget report the gap using the section's presence signal and
continue. No delivery guarantee is needed, only a stated deadline and an
honest account of what missed it: a silent drop would let export claim a
byte-faithful bitstream it did not produce.

Round-trip exact NAL bytes through Annex B, length-prefixed samples, MPEG-TS,
and fMP4. Exercise a video-only subscription, a sidecar-only subscription,
a sidecar arriving after its video frame, reconnect at a group boundary, and
bounded cleanup of sidecar samples whose video frame never arrives.

## Required

- [SEI section](/quest/m2/sei/sei.md) - defines the catalog and correlation
  contract
