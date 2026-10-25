# [S] fMP4 import fills the ambisonic layout

## Goal

An fMP4 Opus track with channel mapping family 2 imports with the catalog's
`ambisonic` field, under the same RFC 8486 rules the Matroska and raw Opus
importers apply in [VR fields](/quest/m2/mux-vr-fields.md).

## Plan

Split from VR fields on 2026-10-22 (maintainer): only this part needs the
`dOps` channel mapping table, so the rest of VR fields does not wait on it.
Read the mapping from the `dOps` that
[CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) carries through the
OpusHead, and reuse VR fields' family 2 validation. Test with an fMP4
first-order ambisonic fixture and a refused non-diegetic stereo one.

Public API: none. Wire: none.

## Required

- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - the `dOps` mapping table an ambisonic fMP4 track needs
- [VR fields](/quest/m2/mux-vr-fields.md) - the ambisonic validation and catalog plumbing this reuses
