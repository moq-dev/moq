# [S] Finish in-band CMAF parameter sets

## Goal

Every path that reads an `avc3`/`hev1` CMAF track (parameter sets in the
samples) handles it, once `moq export fmp4` writes them (#5015) and import
and decode read them (#5037).

## Plan

From #5037:

- MSF: CMAF video from an MSF catalog arrives with no `description`
  (`rs/moq-mux/src/catalog/msf/consumer.rs`), so moq-video refuses it after
  #5037.
  Fill `description` from the init segment's sample entry.
- `moq export h264`/`h265` refuse an imported avc3 track with
  `MissingParamSets` because its avcC has no SPS/PPS. Accept empty sets for
  an in-band track.
- Check moq-gst's `video_caps`: in-band H.265 with no description is labeled
  `stream-format=hev1` (length-prefixed), while a Legacy hev1 track is
  Annex-B (`byte-stream`). Fix the label if it is wrong.

## Required

- [CMAF in-band parameter sets](/quest/m1/cmaf-inline-params.md) - #5037 imports and decodes `avc3`/`hev1` CMAF
- [fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md) - #5015 writes `avc3`/`hev1` entries from the catalog
