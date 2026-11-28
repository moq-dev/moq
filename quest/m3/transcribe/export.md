# [L] Export text renditions as WebVTT and SRT

## Goal

The exporters write every text rendition, whatever its format, as the text
the container carries: an HLS subtitles rendition and a DASH text adaptation
set of WebVTT segments beside the media, a `wvtt` track in fMP4, and
`S_TEXT/WEBVTT` or `S_TEXT/UTF8` (SRT) in Matroska. `moq export vtt` and
`moq export subrip` write one text rendition as a sidecar file, as `moq export
h264` does for a video one; SubRip takes its full name because `moq export
srt` is already the SRT transport. A transcript exports only its final segments;
interim revisions never reach an export.

## Plan

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ every text format: WebVTT,
SRT, and CEA-608/708 where the container carries it (rejected: WebVTT for
transcripts only). This quest covers the text tracks; CEA-608/708 goes back
into the video, which is [its own quest](/quest/m3/transcribe/caption-export.md).

No exporter writes any text rendition on main.
`rs/moq-mux/src/container/fmp4/text.rs` only imports `wvtt` and `stpp`, and
`rs/moq-hls` has no subtitles rendition, so this builds the text path in the
exporters, not just a format conversion. Convert each format to cues once,
in `rs/moq-mux/src/container/text.rs`: `vtt` passes through, `utf8` becomes
cues that each end at the next, `ttml` becomes cues of its text, and
`transcript` becomes one cue per final segment (emitting a WebVTT `<v Speaker N>`
voice span when `speaker` is set). fMP4 also carries `ttml` as
IMSC1 (`stpp`) unchanged, since converting it to WebVTT loses its styling.

A segmented exporter writes a subtitle segment once every cue starting in its
span is final. For a transcript that waits on the segment open at the span's
end, not on its whole utterance, since the transcriber commits at pauses
inside an utterance ([crate](/quest/m3/transcribe/crate.md)); only speech
with no pause at all holds the subtitles back longer. A cue that
crosses a segment boundary repeats in each segment it overlaps, as HLS
requires.

Public API: new `moq export` sinks, and text renditions in each exporter's
output. Wire: none.

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the rendition this converts
