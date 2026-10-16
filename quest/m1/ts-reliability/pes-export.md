# [M] TS export emits whole verbatim PES packets at their placement

## Goal

`moq export ts` reads the `pesPacket` verbatim framing and writes each frame
as the PES packet it holds, byte for byte, sent at the frame's timestamp as
its placement time. A PES without a PTS goes out without one; nothing in its
header is synthesized. Private data placed alongside or after the tune-in
keyframe survives the tune-in rule.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/ts-reliability/README.md)):
a `pesPacket` frame is one complete PES packet, start code through payload,
whose PTS, DTS and every other header field are already on the catalog clock.
Its frame timestamp is the source placement time: the program clock at the
PES's first byte, on the media timeline. This lands before the importer
writes the framing, so the round trip never meets a catalog export can't read.

- **Catalog.** Add `Framing::PesPacket` (`"pesPacket"`) to
  `rs/moq-mux/src/container/ts/catalog.rs`. `streamId` does not apply: the
  stream_id travels in each frame. A catalog naming it alongside
  `pesPacket` is refused.
- **Mux.** Packetize the frame's bytes unchanged on the track's PID: no
  `PesHeader`, no AC-3 repacking (`ac3_frames`
  and the `samples` packing stay only for the legacy `pes` framing). The
  unit is due at its timestamp, the way an unbuffered PID already is. Where
  `verbatim_buffer` models the PID (DVB AC-3, teletext), admission reads the
  PTS from the header bytes.
- **Tune-in.** The rule that drops non-video frames before `video_start`
  compares a verbatim frame's placement with the tune-in keyframe's
  departure, not its PTS. Placement runs ahead of PTS by the decoder delay,
  so comparing against the PTS drops data muxed beside the keyframe.
- **Refusal.** A frame that is not exactly one well-formed PES packet
  (start code, a nonzero `PES_packet_length` matching the frame, a complete
  optional header) fails the track, like a malformed section frame does.
- **Legacy `pes`.** Keep reading it as today, so published importers still
  export. Its special stream IDs (0xBC, 0xBE, 0xBF, 0xF0-0xF2, 0xF8, 0xFF)
  carry no optional header, yet `write_pes` counts one in
  `PES_packet_length`: refuse them at catalog load instead of writing a
  wrong length.

Tests, in `export_test.rs` from hand-built catalogs and frames, wired into
the existing `cargo test` CI: PTS 0 versus no PTS, distinct PTS and DTS,
optional fields (ES rate, trick mode, copy info, CRC, extension, stuffing),
`data_alignment_indicator` clear, private_stream_2 and padding stream IDs,
private data before, beside and after the tune-in keyframe, and between
media. Assert the output PES bytes equal the frame. Extend
`just test ts` with a `--klv` arm that FFmpeg and TSDuck grade once the
[importer](/quest/m1/ts-reliability/pes-import.md) writes the framing, or
leave that arm to it.

Update `drafts/draft-lcurley-moq-mpegts.md` ({{verbatim}}, changelog -02)
with the framing and its timestamp meaning, and the TS rows of
`doc/lib/rs/moq-mux.md`. No JS change: nothing in `js/` reads the section.

## Related

- [Raw stream types](/quest/m1/ts-reliability/stream-type.md) - unknown stream types are the other way a verbatim track fails export
- [T-STD TS export](/quest/m1/tstd/README.md) - the schedule this placement feeds
