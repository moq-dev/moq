# [M] TS export emits whole PES packets at their placement

## Goal

`moq export ts` reads tracks the catalog marks `pesPacket` and writes each
frame as the PES packet it holds, byte for byte, sent at the frame's
timestamp as its placement time. A PES without a PTS goes out without one;
nothing in its header is synthesized. Private data placed alongside or after
the tune-in keyframe survives the tune-in rule.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/ts-reliability/README.md)):
a `pesPacket` frame is one complete PES packet, start code through payload,
whose PTS, DTS and every other header field are already on the catalog clock.
Its frame timestamp is the source placement time: the program clock at the
PES's first byte, on the media timeline. This lands before the importer
writes the member, so the round trip never meets a catalog export can't read.

- **Catalog.** Add an optional `pesPacket` member to the `mpegts.tracks`
  entry in `rs/moq-mux/src/container/ts/catalog.rs`, beside `verbatim`,
  holding the PMT `streamType` (for example `catalog::PesPacket`). It is a
  member rather than a `framing` value so old exporters ignore the entry
  instead of refusing the catalog. The stream_id travels in each frame, so
  it has no `streamId`. An entry with both `verbatim` and `pesPacket` is
  refused.
- **Mux.** Packetize the frame's bytes unchanged on the track's PID: no
  `PesHeader`, no AC-3 repacking. The unit is due at its timestamp, the way
  an unbuffered PID already is. Where `verbatim_buffer` models the PID (DVB
  AC-3, teletext), admission reads the PTS from the header bytes. That PTS
  covers only the first AC-3 sync frame, so the model walks the sync frames
  in the bytes and removes each at its own decode time, as `ac3_frames`
  does today, without rewriting the PES.
- **Tune-in.** The rule that drops non-video frames before `video_start`
  compares a `pesPacket` frame's placement with the tune-in keyframe's
  departure, not its PTS. Placement runs ahead of PTS by the decoder delay,
  so comparing against the PTS drops data muxed beside the keyframe.
- **Refusal.** A frame that is not exactly one well-formed PES packet
  (start code, a `PES_packet_length` matching the frame, a complete
  optional header where the stream_id has one) fails the track, like a
  malformed section frame does. A length of zero is allowed only for a
  video stream_id (0xE0-0xEF), as H.222.0 permits.

Leave the legacy `pes` framing as it is here; the
[importer quest](/quest/m1/ts-reliability/pes-import.md) deletes it once
nothing writes it.

Tests, in `export_test.rs` from hand-built catalogs and frames, wired into
the existing `cargo test` CI: PTS 0 versus no PTS, distinct PTS and DTS,
optional fields (ES rate, trick mode, copy info, CRC, extension, stuffing),
`data_alignment_indicator` clear, private_stream_2 and padding stream IDs,
private data before, beside and after the tune-in keyframe, and between
media, and an entry naming both members. Assert the output PES bytes equal
the frame.

Update `drafts/draft-lcurley-moq-mpegts.md` (the `Track` type, the "MUST
ignore an entry" rule, {{verbatim}}, {{rebuild}}, changelog -01) with the
member, its timestamp meaning, and the fact that a consumer that predates
it drops the track, plus the TS rows of `doc/lib/rs/moq-mux.md`. No JS
change: nothing in `js/` interprets the section.

## Related

- [Raw stream types](/quest/m1/ts-reliability/stream-type.md) - unknown stream types are the other way a verbatim track fails export
- [T-STD TS export](/quest/m1/tstd/README.md) - the schedule this placement feeds
