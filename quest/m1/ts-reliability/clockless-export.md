# [M] TS export rebuilds programs without video

## Goal

`moq export ts` rebuilds an audio-only program carrying SCTE-35 or other
private sections, and a program of only `verbatim` or `pesPacket` tracks,
instead of refusing it. Basic opaque carriage does not wait for the typed
cue API.

## Plan

Found 2026-10-20 in the fidelity audit (`Export::build_psi`):

- Any section-framed track fails the export with "requires a video track
  for the program clock" unless a video track exists, even when an audio
  track could carry the PCR (`scte35_without_video_export_is_rejected`
  asserts it).
- With no video or audio track the PCR has no PID, so a `pesPacket`-only
  program (KLV, teletext) or a sections-only one fails "requires a video or
  audio track for the PCR".
- The import side already carries these programs: sections and whole PES
  are placed by the source PCR, whatever PID carried it.

Find why the video requirement exists before removing it: sections may lean
on the video tune-in point (`video_start`) or the video decode clock. Then
take the PCR from the first audio track when there is no video, and from a
PCR-only PID when there is no media at all. The schedule already writes a PCR
in an adaptation-only packet when no clock-PID packet can open the slot. A
data-only program has no tune-in point, so its first frame starts the output.

Rust: `rs/moq-mux` TS export. JS: none. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): the rebuild section says a
consumer picks a PCR PID for a program without media and MUST NOT refuse one
for lacking video.

Tests: replace `scte35_without_video_export_is_rejected` with round trips for
audio plus SCTE-35, audio plus `pesPacket`, sections only, and `pesPacket`
only, each through import and export with byte-exact section and PES
payloads and placements. Add a generated audio-plus-SCTE-35 arm to the
TSDuck round trip in `test/ts`, so `compliance.py` grades PCR repetition and
the PMT on a program whose clock rides audio or its own PID.

## Related

- [Typed SCTE-35](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - the browser cue API builds on this carriage
