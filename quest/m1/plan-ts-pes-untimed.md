# [M] Plan: private PES timing and header fidelity

## Goal

Decide how a verbatim MPEG-TS track carries a PES that has no PTS without
inventing a presentation time, from import through export, and which PES
header semantics it retains. The usual case is asynchronous private data
such as KLV. Produce focused implementation quests with explicit supported
profiles and refusal rules.

## Plan

Today the importer stamps a PES without a PTS at the track's live edge, or
at 0 before the first frame. `drafts/draft-lcurley-moq-mpegts.md` says 0.
Both are invented times, which the untimed model ([#4822](https://github.com/moq-dev/moq/pull/4822)) rules out for
moq-net itself.

Timedness is per track since 2026-10-05: a timed track refuses an untimed
frame, so a PES without a PTS can only go out untimed on an untimed track.
Weigh that in the carriage decision.

An untimed track isn't enough on its own. Verbatim
tracks use the legacy container, whose payload always starts with a
timestamp, and its decoder reads the time from there, not from the frame. So
the decision covers:

- **Carriage:** either the legacy payload keeps a time, or verbatim tracks
  move to a container that takes its time from the moq-net frame. Weigh what
  the second costs existing catalogs and consumers of the `mpegts` section.
- **Export:** how the TS exporter schedules and writes an untimed PES, which
  goes out with no PTS.
- **Stream types:** which ones this applies to. A media elementary stream
  may split an access unit across PES packets or infer timing. There, a
  missing PTS doesn't mean untimed.

The maintainer decides the carriage, because it touches the catalog and
container contract.

The 2026-10-11 audit expands the decision to avoid settling timestamps in
isolation. A source placement time used for mux scheduling is distinct from
optional PTS and DTS. Today's private-PES track retains payload, `stream_id`,
and present PTS as the frame timestamp, but not the original DTS or other
PES-header fields. The parser skips ESCR, ES rate, trick mode, copy information, CRC and extension
fields; export synthesizes a generic PTS header and cannot reproduce all
special stream-ID header forms. A track called verbatim must say which bytes
or semantics that means. Unknown stream types also need to survive export or
be refused at import, not fail later through an enum conversion.

Settle per-PES timing presence, scheduling of asynchronous data alongside
advancing media, header preservation versus normalization, and the legacy
container migration together. The exporter currently drops non-video frames
before the initial video time; demonstrate that permitted untimed/private
content is not lost through that rule. Keep the catalog/container decision
separate from raw whole-TS fallback, whose packets already contain the headers.

Automated CI tests for the implementation quests: importer to encoded track
to exporter for missing PTS versus real PTS 0, distinct PTS/DTS, asynchronous
private data before and between media, optional headers, and special stream
IDs. Fixtures assert either retained semantics or the precise supported-profile
refusal, using TSDuck/FFmpeg where applicable. Update Rust, JS, the MPEG-TS
draft and the guide for any catalog/container wire change.
