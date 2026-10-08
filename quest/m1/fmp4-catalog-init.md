# [M] fMP4 export inits from the catalog

## Goal

`moq export fmp4` writes its init segment at the first catalog for an Annex-B
H.264 or H.265 source, instead of waiting for the first keyframe's SPS, and the
samples carry their parameter sets in-band.

## Plan

Decided (2026-10-04), carried over from the export track-set work:

- Write avc3 and hev1 sample entries whose configuration record comes from the
  catalog's codec string, with SPS and PPS (and VPS) in-band in the samples.
  The Annex-B transform must then keep and length-prefix the parameter sets
  rather than strip them, and re-inject the cached set on a keyframe that
  lacks one.
- Only when the catalog carries everything the record needs. avcC for a high
  profile (110, 122, 244, ...) and hvcC for anything beyond Main or Main Still
  Picture hold chroma format and bit depths that the codec string does not
  carry; those tracks keep waiting for their SPS through the bounded pre-init
  queue `fmp4::Export` already has. Dimensions missing from the catalog also
  still wait.
- Apple prefers hvc1 for HEVC and some editors handle avc3 poorly; that is the
  accepted trade.

Today a returning Annex-B rendition is compared by its avcC/hvcC, SPS bytes
included; with in-band parameter sets the comparison covers only the
catalog-derived record, so an encoder that restarts with a new SPS can return.

## Related

- [CMAF frame timestamp](/quest/m1/cmaf-frame-timestamp.md) - #4826 edits the same fMP4 code; whichever lands second rebases, with no forced order
- [Finish in-band CMAF parameter sets](/quest/m1/cmaf-inline-followups.md) - the readers that must handle the avc3/hev1 tracks this writes
