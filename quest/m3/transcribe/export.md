# [L] Export transcripts as WebVTT

## Goal

The HLS and fMP4 exporters write a transcript rendition's final segments as
WebVTT: an HLS subtitles rendition beside the media playlists, and a `wvtt`
track in fMP4. Interim revisions never reach an export.

## Plan

Split out of [the format](/quest/m3/transcribe/format.md): no exporter
writes any text rendition on main. `rs/moq-mux/src/container/fmp4/text.rs`
only imports `wvtt` and `stpp`, and `rs/moq-hls` has no subtitles rendition,
so this builds the text path in both exporters, not just a format
conversion.

Public API: none beyond the exporters' output. Wire: none.

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the rendition this converts

## Waiting

- Maintainer: pick which text renditions the exporters write. Options: every
  text format (`vtt` passed through, `utf8` and `transcript` converted to
  WebVTT, `ttml` as IMSC1 in fMP4), so imported captions, including those the
  importer will take from CEA-608/708 SEI, export too; or transcripts only, as
  planned. Recommendation: every text format, since the subtitles rendition
  and segmenting are the bulk of the work either way. An agent cannot take
  it, since it widens planned scope.
