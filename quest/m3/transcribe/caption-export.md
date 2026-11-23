# [L] Re-insert captions into exported video

## Goal

Exporting to a container whose video carries captions in band (MPEG-TS, FLV,
and fMP4, including HLS and DASH segments) writes the broadcast's
`role: "caption"` text renditions back into the H.264 or H.265 video as
CEA-608/708 `cc_data` in A/53 SEI, so a broadcast chain, an RTMP ingest, or a
TV decoder downstream sees the captions it expects. The HLS playlist declares
them with `CLOSED-CAPTIONS`, and the DASH manifest with its CEA-608
accessibility descriptor.

## Plan

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ the exporters write every
text format, CEA-608/708 included where the container carries it (rejected:
WebVTT for transcripts only). The text tracks themselves are
[export](/quest/m3/transcribe/export.md)'s.

The importer strips caption SEI from the video
([caption import](/quest/m3/transcribe/caption-import.md)), so this is the
only way captions reach a container that has no text track of its own.
Encode from the cues [export](/quest/m3/transcribe/export.md) converts each
format to, so an imported caption track, a WebVTT upload, and a live
transcript's final segments all re-insert alike. Imported captions
round-trip their text, rows, and timing, not every 608 style code.

Recommendation: write roll-up for a transcript and for `utf8`, whose text
arrives as it is spoken, and pop-on for cue formats; give the first caption
rendition CC1 and 708 service 1, and the next CC3 and service 2, in catalog
order, carrying the language in the HLS `CLOSED-CAPTIONS` entry and, for
MPEG-TS, a caption service descriptor. Pace the `cc_data` triples at the
frame rate's budget (A/53's `cc_count`), and queue text that exceeds it
rather than drop it, which roll-up absorbs by delaying its words.

Insert an SEI NAL into the access unit at the captions' presentation time,
or, when a cue arrives after that frame went out, into the next frame
written, as live broadcast captions lag the speech; no cue is dropped for
arriving late. A transcript's final lands a pause after its speech
([crate](/quest/m3/transcribe/crate.md)), so a container sink's existing
release delay (`--delay`) is the caption budget: a cue inside it lands on
time, and an export that wants every caption aligned raises it past the
transcript's lag. A cue that missed its frame starts from the next one,
ahead of later cues, so lateness shifts captions but never reorders them.
Do this in the exporter's codec layer (`rs/moq-mux/src/codec/h264/export.rs`,
`rs/moq-mux/src/codec/h265/export.rs`), reusing the SEI writer
[caption import](/quest/m3/transcribe/caption-import.md) adds to rewrite
NALs. Matroska is left out, since its own text tracks carry the captions
([export](/quest/m3/transcribe/export.md)).

Public API: none beyond the exporters' output. Wire: none.

## Required

- [Import CEA-608/708 captions as text tracks](/quest/m3/transcribe/caption-import.md) - the A/53 SEI framing and 608/708 codec this encodes with
- [Export text renditions as WebVTT and SRT](/quest/m3/transcribe/export.md) - the cue conversion each text format goes through
