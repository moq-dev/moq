# [M] TS import carries private PES whole at its source placement

## Goal

`moq import ts` publishes every PES-framed verbatim stream (private data
such as KLV, teletext, DVB subtitles, DVB AC-3, unknown types) as
`pesPacket` frames: the whole PES packet with its clock fields moved onto the
catalog clock, timestamped with its source placement. A PES without a PTS
keeps none, and no frame is stamped 0 or at the track's own edge. Streams
the profile cannot carry are refused and reported, never silently altered.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/ts-reliability/README.md)).
Today `VerbatimStream::write` stamps a PES without a PTS at its own track's
`live_edge()`, or `Timestamp::ZERO` before any frame. A stream that never
carries a PTS (asynchronous KLV) therefore sits at 0 forever, below
`video_start`, and export drops all of it. Reproduce that with a failing
round-trip test first.

- **Placement.** The frame timestamp is the program clock at the PES's first
  byte, interpolated between the PCRs around it by packet position
  (ISO 13818-1 2.4.2.2), shifted onto the catalog clock by the timebase
  offset that shifts the PTS. A PES waits for the next PCR, within the
  existing `Retained` bound; it never spans a PCR discontinuity. The value
  depends only on the input bytes, so same-epoch importers agree. A program
  with no PCR PID cannot place its PES, so its verbatim PES streams are
  refused.
- **Bytes.** The frame is the PES packet as received, start code through
  payload. PTS and DTS are rewritten in place onto the catalog clock modulo
  2^33, exactly as the media PTS moves. Every other field stays byte-exact,
  including flags, `data_alignment_indicator`, ES rate, trick mode, copy
  info, the PES CRC (it covers the previous packet's data, which is
  unchanged), extension data, stuffing, and the special stream-ID forms
  that have no optional header. Each PES keeps its own stream_id, so the
  catalog stops recording `streamId` for this framing.
- **Refusal.** A PES carrying a clock this does not rebase (ESCR, a
  `pack_header_field`, TREF) or PES-level scrambling refuses its stream:
  no track is advertised, or an advertised one ends with an error and loses
  its catalog entry, with a warning and an importer health count. Rebase
  ESCR instead if a real source needs it, with a fixture.
- **Scope.** This covers every stream the importer carries verbatim as PES,
  whatever its stream type, because nothing is inferred: a media stream
  that splits an access unit across PES, leaving later ones without a PTS,
  round-trips the same way. Decoded codecs keep their own timing.

Tests: generated fixtures (extend `test/ts/fragment_pes.py` or add a
generator) for asynchronous KLV (stream_type 0x06, `KLVA` registration, no
PTS) and synchronous KLV (stream_type 0x15, metadata_stream 0xFC, PTS),
importer to track to exporter, asserting byte equality of each PES outside
its rebased PTS and DTS. Cover PTS 0 versus no PTS, distinct PTS and DTS,
data before the first video and between media, optional fields, special
stream IDs, and each refusal. Add the `just test ts --klv` arm with FFmpeg
and TSDuck grading, run on PRs that touch `rs/moq-mux/src/container/ts`.

Update `drafts/draft-lcurley-moq-mpegts.md` (the importer now writes
`pesPacket` and states what it refuses) and the TS lines of
`doc/bin/cli.md` and `doc/lib/rs/moq-mux.md`.

## Required

- [Whole-PES export](/quest/m1/ts-reliability/pes-export.md) - export reads `pesPacket` before import writes it
