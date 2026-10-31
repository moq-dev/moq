# [S] fMP4 import fills the ambisonic layout

## Goal

An fMP4 Opus track with channel mapping family 2 imports with the catalog's
`ambisonic` field, under the same RFC 8486 rules the Matroska and raw Opus
importers apply (`opus::Mapping::new` refuses the family 2 channel counts the
catalog cannot carry, and the OpusHead's `AudioConfig` sets the layout).

## Plan

Split from the VR fields quest on 2026-10-22 (maintainer), which landed the
Matroska and raw Opus side: only this part needs the `dOps` channel mapping
table.
Read the mapping from the `dOps` that
[CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) carries through the
OpusHead, and reuse that family 2 validation. Test with an fMP4
first-order ambisonic fixture and a refused non-diegetic stereo one.

Noted 2026-10-31 from [moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308),
which fills `projection`, `stereo`, and `layout` on import only
(`rs/moq-mux/src/codec/spatial.rs`): no exporter writes them back yet. The
fMP4 exporter should write `sv3d`/`st3d` and a family 2 `dOps`, and the
Matroska exporter `Projection` and `StereoMode`, so an imported VR track
round-trips. Do the fMP4 side here if it stays small; otherwise split the
exporters into their own quest.

Public API: none. Wire: none.

## Required

- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - the `dOps` mapping table an ambisonic fMP4 track needs
