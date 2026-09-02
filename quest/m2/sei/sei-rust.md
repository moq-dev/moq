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

The exporter joins by video frame timestamp and restores prefix and suffix
placement and ordering. It reinserts what it has; a sidecar sample it never
received is a logged loss, not a stall, because export has no live decoder
waiting on it.

Round-trip exact NAL bytes through Annex B, length-prefixed samples, MPEG-TS,
and fMP4. Exercise a video-only subscription, a sidecar-only subscription,
reconnect at a group boundary, and bounded cleanup of sidecar samples whose
video frame never arrives.

## Required

- [H.265 suffix SEI](/quest/m0/h265-suffix.md) - fixes access-unit ownership
  before the splitter makes that ownership a sidecar contract
- [SEI section](/quest/m2/sei/sei.md) - defines the catalog and correlation
  contract
