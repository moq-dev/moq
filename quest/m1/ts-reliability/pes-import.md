# [L] TS import carries private PES whole at its source placement

## Goal

`moq import ts` publishes every PES stream it does not decode (private data
such as KLV, teletext, DVB subtitles, DVB AC-3, unknown types) as a
`pesPacket` track: each frame the whole PES packet with its clock fields
moved onto the catalog clock, timestamped with its source placement. A PES
without a PTS keeps none, and no frame is stamped 0 or at the track's own
edge. Streams the profile cannot carry are refused and reported, never
silently altered. The legacy `pes` framing is gone from import, export and
the draft.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/ts-reliability/README.md)).
Today `VerbatimStream::write` stamps a PES without a PTS at its own track's
`live_edge()`, or `Timestamp::ZERO` before any frame. A stream that never
carries a PTS (asynchronous KLV) therefore sits at 0 forever, below
`video_start`, and export drops all of it. Reproduce that with a failing
round-trip test first.

- **Placement.** The frame timestamp is the program clock at the PES's first
  byte, interpolated between the PCRs on either side by byte position
  (ISO 13818-1 2.4.2.2): the PES start code's input offset against each
  PCR's reference byte, not packet boundaries, reusing the byte-offset
  clock in `passthrough.rs`, with the PCR unwrapped into the era of the
  anchored PTS (the 2^33 multiple nearest it, since PTS leads PCR by well
  under half a wrap) so a start near the wrap does not split them, shifted onto the catalog clock by the timebase
  offset that shifts the PTS. A PES waits for the next PCR, within the
  existing `Retained` bound; it never spans a PCR discontinuity. The value
  depends only on the input bytes and the timebase offset, so it adds no
  disagreement of its own; same-epoch importers agree once the
  [epoch anchor](/quest/m2/epoch-anchor.md) fixes that offset. A program
  with no PCR PID cannot place its PES, so its private PES streams are
  refused. The timebase offset is anchored only by a PTS today, so a PES
  also waits for that anchor within the same bound; a program whose PES
  never carry a PTS (data only) is refused and tested, unless the PCR can
  anchor the timebase without breaking same-epoch agreement.
- **Trailing PES.** A PES that starts after its timebase's last PCR (end of
  input, or a PCR discontinuity before the next PCR) has no PCR on the far
  side. Place it by extrapolating from that timebase's last two PCRs, which
  is still deterministic, rather than hold or drop it. With fewer than two
  PCRs it is dropped and counted in importer health. A PES before the
  first PCR the importer sees is dropped and counted the same way, like
  anything before acquisition: back-extrapolating would let importers
  started apart disagree. A new timebase starts with its first PCR, so
  this happens only at startup. Test each case.
- **Bytes.** The frame is the PES packet as received, start code through
  payload. PTS and DTS are rewritten in place onto the catalog clock modulo
  2^33, exactly as the media PTS moves. Every other field stays byte-exact,
  including flags, `data_alignment_indicator`, ES rate, trick mode, copy
  info, the PES CRC (it covers the previous packet's data bytes, not its
  header, so rebasing leaves it valid; assert that with a CRC fixture), extension data, stuffing, and the special stream-ID forms
  that have no optional header. Each PES keeps its own stream_id.
- **Catalog.** The entry records `pid`, `descriptors` and the `pesPacket`
  member that [export](/quest/m1/ts-reliability/pes-export.md) reads, and
  no `verbatim` record. A published exporter ignores such an entry and
  silently leaves the stream out of its TS; that is the accepted cost.
- **Refusal.** A PES carrying a clock this does not rebase (ESCR, a
  `pack_header_field`, TREF), PES-level scrambling, or a zero
  `PES_packet_length` on a non-video payload, by export's rule
  refuses its stream:
  no track is advertised, or an advertised one ends with an error and loses
  its catalog entry, with a warning and an importer health count.
- **Scope.** This covers every stream the importer carries undecoded as
  PES, whatever its stream type, because nothing is inferred: a media
  stream that splits an access unit across PES, leaving later ones without
  a PTS, round-trips the same way. Decoded codecs keep their own timing.
- **Delete `pes`.** In the same change, so main never writes a catalog its
  exporter refuses: drop `Framing::Pes` and `Verbatim::stream_id`, export's
  `write_pes` path for verbatim tracks, and the DVB AC-3 repack
  (`queue_ac3` and the per-frame PES it writes). A catalog naming
  `framing: "pes"`, or a `verbatim` record with no `framing` (which meant
  `pes` in -00), is refused: never reinterpret it as a section. Published
  importers and existing recordings with private PES then fail to export,
  loudly. Migrate the hand-built `pes` catalogs in tests, including
  `rs/moq-cli/src/publish.rs`. The repack rewrites a multi-frame AC-3 PES
  as one PES per sync frame; only that output rewrite goes. The PES PTS
  covers only its first sync frame, so the export's AC-3 buffer model still
  walks the sync frames in the unchanged bytes to remove each at its own
  decode time, and the multi-frame AC-3 fixture below passes the strict
  T-STD check.

Tests: generated fixtures (extend `test/ts/fragment_pes.py` or add a
generator) for asynchronous KLV (stream_type 0x06, `KLVA` registration, no
PTS) and synchronous KLV (stream_type 0x15, metadata_stream 0xFC, PTS),
importer to track to exporter, asserting byte equality of each PES outside
its rebased PTS and DTS. Cover a start straddling the 33-bit wrap, PTS 0 versus no PTS, distinct PTS and DTS,
data before the first video and between media, optional fields, special
stream IDs, DVB AC-3 with several sync frames per PES, each refusal, and
the refused `pes` catalog. Add a `just test ts --klv` arm with FFmpeg and
TSDuck grading, run on PRs that touch `rs/moq-mux/src/container/ts`.

Update `drafts/draft-lcurley-moq-mpegts.md` (the importer writes
`pesPacket` and states what it refuses; `framing` is `"section"` only and
required, `streamId` removed, changelog -01 noting the break with -00) and
the TS lines of `doc/bin/cli.md` and `doc/lib/rs/moq-mux.md`.

## Required

- [Whole-PES export](/quest/m1/ts-reliability/pes-export.md) - export reads `pesPacket` before import writes it
