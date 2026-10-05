# [S] Carry surround Opus through CMAF

## Goal

An fMP4 Opus track whose `dOps` declares channel mapping family 1 imports with
its mapping table in the OpusHead description, and a family 1 description
exports to a `dOps` that keeps the table. Today both directions refuse it.

## Plan

Build the head from the `dOps` family and table with `opus::Mapping` on import,
and write the table from the parsed head on export in place of the
`UnsupportedMappingFamily` refusal in `synthesize_audio_trak`. Keep refusing
families the head cannot describe.

Regression: a family 1 5.1 `dOps` round-trips through import and export with
its table intact.

## Required

- [mp4-atom dOps mapping](/quest/m1/mp4-atom-dops-mapping.md) - `Dops` carries the mapping family and table
