# [XS] fMP4 import fills the ambisonic layout

## Goal

An fMP4 Opus track with channel mapping family 2 imports with the catalog's
`ambisonic` field, under the same RFC 8486 rules the Matroska and raw Opus
importers apply (`opus::Mapping::new` refuses the family 2 channel counts the
catalog cannot carry, and the OpusHead's `AudioConfig` sets the layout).

## Plan

Split from the VR fields quest on 2026-10-22 (maintainer), which landed the
Matroska and raw Opus side: only this part needs the `dOps` channel mapping
table.
The fMP4 importer carries the `dOps` mapping table into the OpusHead since
CMAF surround Opus (`fmp4::opus_mapping`), so a family 2 `dOps` should
already import as `ambisonic` through that family 2 validation. What remains
is proving it: test with an fMP4 first-order ambisonic fixture and a refused
non-diegetic stereo one.

Public API: none. Wire: none.
