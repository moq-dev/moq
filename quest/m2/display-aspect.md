# [S] Every video importer reports the display aspect

## Goal

A video rendition with non-square samples gets the catalog display aspect
from its bitstream, whichever importer reads it:

- H.264 (the VUI sample aspect)
- H.265 from both Annex-B and an `hvcC` record
- AV1, if it carries a stream-level signal

A square-sample stream still leaves the fields unset, as the encode hint does
today.

## Plan

The known-SPS paths now share their metadata: H.264 reads predefined and
extended VUI sample aspect through Annex-B and avcC; HEVC uses the same SPS/VUI
resolver for Annex-B and hvcC. Matroska and fMP4's avc1, avc3, hvc1 and hev1
initializers also take that metadata. Square samples leave the display aspect
unset, so the existing hint remains a fallback. The FFI's existing dimensions
and aspect fields are documented.

AV1's sequence header carries no sample aspect. Its
[render size](https://aomediacodec.github.io/av1-spec/#render-size-syntax) is a
per-frame hint that may inherit reference-frame dimensions, so it does not
establish a fixed aspect for the rendition.

A scope decision remains: valid avc3/hev1 initializers may omit parameter sets
and carry the SPS only inside later CMAF media. The passthrough importer does
not inspect those payloads, so their bitstream aspect stays unknown. Decide
whether that acquisition belongs here or in a follow-up before closing this
quest. The current fix preserves headerless initialization and does not add
media-payload inspection.

Tests cover non-square and square SPS fixtures through the codec, Matroska and
all four fMP4 paths, plus headerless avc3/hev1 controls. The metadata regressions
fail before their fixes.

Public API: no shape change; existing FFI fields gain docs. Wire: existing
catalog aspect and fixed-frame-rate fields are populated in more cases.

## Related

- [Catalog colour](/quest/m2/color-catalog.md) - parses the same H.264/H.265 VUI for colour
