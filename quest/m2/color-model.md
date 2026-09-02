# [M] Catalog colour model

## Goal

The Hang catalog describes a video rendition's colour: primaries, transfer
characteristics, matrix coefficients, range, and the HDR10 mastering display and
content light levels. A renderer can set up its pipeline from the catalog
instead of guessing or reparsing the bitstream.

## Plan

`rs/hang/src/catalog/video/mod.rs` has carried a bare `// TODO color space` since
the config was written. Two codecs already expose colour per their own syntax:
`VP9` carries primaries, transfer characteristics, matrix coefficients, and
range inside its codec string, and `AV1` has the same fields, which
`rs/moq-mux/src/codec/av1/import.rs` fills from the sequence header. They only
round-trip through muxers (the fMP4 exporter writes VP9's into `vpcC`); no
renderer consumes either, H.264 and H.265 have no equivalent, and there is no HDR10,
mastering display, or content light handling in `rs/` or `js/` at all. So an
HDR broadcast is delivered today and rendered as if it were SDR, whatever the
source signalled.

Model the codec-neutral properties rather than one codec's syntax, so H.264 and
H.265 VUI, AV1 colour config, and a container's `colr` box all populate the same
fields. Generalize the existing VP9 and AV1 fields into that model rather than
adding a second colour source beside them: the codec-specific fields keep their
wire meaning (the VP9 codec string is defined by its spec) and feed the neutral
fields at import, so a renderer reads one place. Fill them at import from
whichever source the rendition provides, and emit them on export.

Settle the precedence before adding the fields, because these sources disagree
in real files and a renderer must not see a different answer per import path.
Prefer the bitstream over the container: VUI and the AV1 colour config are what
the encoder actually signalled and travel with the elementary stream, while a
`colr` box is added by a muxer that may be wrong or stale. Record which source
won so a conflict is diagnosable rather than invisible, and treat an
unspecified value as absent rather than as a signalled default, so a partial
container box does not override a complete bitstream.

Static display properties belong here rather than in a timed track, which is
also where the HDR10 metadata that lives in H.26x SEI should land once it can be
read. That is the one seam with the SEI line, and it runs in one direction:
this quest gives display metadata a home, and does not depend on SEI work.

Test each source of truth in isolation, a source that signals nothing, a
conflict between VUI and container resolving to the bitstream, a container box
that fills a gap the bitstream left unspecified, and an SDR round trip that
stays byte-identical.

## Related

- [SEI sidecars](/quest/m2/sei/README.md) - moves SEI out of the video track;
  the display metadata inside it needs the home this quest builds
