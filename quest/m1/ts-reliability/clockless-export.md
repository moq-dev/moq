# [M] TS round trip of programs without video

## Goal

`moq import ts` carries, and `moq export ts` rebuilds, an audio-only program
carrying SCTE-35 or other private sections, and a program of only `verbatim`
or `pesPacket` tracks whose PMT names a PCR PID, instead of refusing it. A
data-only program without a PCR PID stays refused: nothing places its data. Basic opaque carriage does not
wait for the typed cue API.

## Plan

Found 2026-10-20 in the fidelity audit (`Export::build_psi`, `Import::finish`):

- Any section-framed track fails the export with "requires a video track
  for the program clock" unless a video track exists, even when an audio
  track could carry the PCR (`scte35_without_video_export_is_rejected`
  asserts it).
- With no video or audio track the PCR has no PID, so a `pesPacket`-only
  program (KLV, teletext) or a sections-only one fails "requires a video or
  audio track for the PCR".
- Import carries audio plus sections or `pesPacket`: the audio PTS anchors
  the timebase, and sections and whole PES are placed by the source PCR,
  whatever PID carried it. A data-only program fails instead: placed units
  wait in `release` for a media PTS to anchor the timebase, and
  `Import::finish` ends with "reached end of input without a media
  timestamp".

Import: when the PMT lists no media stream but names a PCR PID, anchor the
timebase at the first PCR, so placement alone puts the data on the catalog clock. A media stream a
later PMT adds shares that anchor, since PCR and PTS run on one program
clock. The era must come from the PCR too: `placement()` reduces its
unwrapped ticks `% PCR_WRAP`, and `PtsUnwrap::pcr` picks the era from the
last media PTS, so without one a data-only program restarts near zero at the
26.5-hour PCR wrap. Keep the PCR era across the wrap without a PTS, and let a
media stream added later unwrap its PTS against that era.

Find why the video requirement exists before removing it: sections may lean
on the video tune-in point (`video_start`) or the video decode clock. Then
take the PCR from the first audio track when there is no video, and from a
PCR-only PID when there is no media at all. The schedule already writes a PCR
in an adaptation-only packet when no clock-PID packet can open the slot. A
data-only program has no tune-in point, so its first frame starts the output.

Rust: `rs/moq-mux` TS import and export. JS: none. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): the rebuild section says a
consumer picks a PCR PID for a program without media and MUST NOT refuse one
for lacking video.

Tests: replace `scte35_without_video_export_is_rejected` with round trips for
audio plus SCTE-35, audio plus `pesPacket`, sections only, and `pesPacket`
only, each through import and export with byte-exact section and PES
payloads and placements. A data-only import crosses the PCR wrap with
continuous timestamps, from a generated PCR that starts just before the
wrap. Keep a refusal test for a data-only program whose PMT has no PCR
(`PCR_PID` 0x1FFF): its section and whole-PES streams count in `refused`
and nothing publishes. Add generated audio-plus-SCTE-35 and sections-only arms to the
TSDuck round trip in `test/ts`, so `compliance.py` grades `PCR_PID`, PCR
repetition and the PMT both when the clock rides audio and when it rides
its own PID.

## Related

- [Typed SCTE-35](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - the browser cue API builds on this carriage
