# [M] ID3 catalog section

## Goal

A Hang catalog exposes ID3 metadata in a top-level `id3` section with named
tracks and media correlation, independent of the MPEG-TS PID that supplied it.
Consumers can discover and read timed ID3 without understanding the `mpegts`
extension, while MPEG-TS export still reconstructs a valid ID3 elementary
stream.

## Plan

Define the catalog section and frame contract for complete ID3 tags. Carry
original tag bytes, track identity, source presentation time when present,
the media rendition or timeline the tag accompanies, and the raw stream-level
PMT descriptor loop plus any application or program registration metadata the
exporter needs to reproduce the input. Do not parse the tag into a fixed
metadata vocabulary; applications can decode frames with their ID3 library and
new tag types remain forward-compatible.

Teach MPEG-TS import to use this section only when the PMT descriptors or
application metadata identify ID3 and complete payloads validate as ID3 tags.
Stream type `0x15` alone is generic metadata PES and does not qualify.
Ambiguous, non-ID3, or inconsistent streams stay byte-faithful in the generic
`mpegts` representation. Export typed ID3 to recreate the elementary stream,
descriptors, and timestamps, without publishing a second generic copy of a
stream positively identified as ID3.

Land matching Rust and web catalog bindings plus byte-faithful round-trip tests
with default and non-default PMT descriptors, multiple tags, unknown frames,
large tags spanning PES packets, timestamp wrap, discontinuity, and an ID3-only
program. Include non-ID3 and malformed stream type `0x15` fixtures that remain
generic.

## Related

- [SRT metadata parity](/quest/m2/srt-metadata.md) - independently preserves
  generic MPEG-TS metadata through the SRT gateway
- [SEI sidecars](/quest/m2/sei/README.md) - the separate codec metadata
  contract
