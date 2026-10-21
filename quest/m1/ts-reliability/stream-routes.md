# [M] Every TS PID gets a declared route

## Goal

TS import gives every PID it reads one declared route: decoded, whole-PES
`pesPacket`, `verbatim` section, captured PSI/SI, or not carried. A PID the
semantic lane does not carry shows in `ts::stats` with a count and one
warning, so an operator sees what to move to the raw lane. A known
`stream_type` never implies carriage it does not get.

## Plan

Found 2026-10-20 in the fidelity audit (`Import::ensure_stream`,
`Import::decode`):

- **MPEG-1/2 video (0x01/0x02)** is `Stream::Clock`: its PTS anchors the
  timebase and its payload is discarded, so TS to Hang to TS loses the
  program's video without an error, while an unknown `stream_type` survives
  as `pesPacket`. Decided 2026-10-20 by the maintainer: carry it as an
  opaque `pesPacket` and keep its clock role, so the round trip keeps the
  video and a Hang player still sees no rendition it cannot decode. The
  draft already lets a video PES leave `PES_packet_length` unbounded.
  Rejected: refusing it loudly and leaving the video to the raw lane, which
  drops a stream the semantic lane can carry unchanged.
- **Section-framed types outside 0x05 and CUEI-marked 0x86** are PES-parsed.
  The ISO 13818-6 DSM-CC types (0x0A to 0x0D) ride sections, so every unit
  fails the start code and counts as `damaged`. Route them through
  `ensure_section`; confirm each type against 13818-6 first.
- **0x86 without CUEI** drops with a warning and no count. Count it in
  `refused`, since it is ambiguous by design.
- **LATM/LOAS AAC (0x11)** already rides `pesPacket` with the `mpegts`
  section, though the `Import` docs and the `ensure_stream` comment say it is
  dropped. Fix the docs to match.
- **PIDs outside the PMT and the captured SI** (CAT on 0x0001 and the
  EMM/ECM PIDs it or a CA descriptor names, TSDT, ATSC PSIP on 0x1FFB and the
  PIDs its MGT signals, the DVB RST/DIT/SIT PIDs, and any PID no table lists)
  drop silently. Count each in a `ts::stats` row; null packets stay
  uncounted. ATSC PSIP and conditional access are outside the semantic
  profile; the guide points them at the raw lane.
- **TS-scrambled packets** on an elementary PID become PES parse damage.
  Count them in `refused` instead, since the semantic lane cannot carry them
  and the raw lane can.

Prefer the existing `stats::Stream` row and its `refused` count over a new
field; if a dropped PID needs a new `Class` or member, report the public API
change. Coordinate the serialized shape with
[TS health counters](/quest/m2/ts-health-stats.md).

Rust: `rs/moq-mux` TS import (and export for MPEG-2 video `pesPacket`
packetization). JS: none. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): list MPEG-2 video among the
`pesPacket` examples and the DSM-CC types among the section-framed ones.

Tests: generated `fixture::Mux` inputs with MPEG-2 video, each DSM-CC type,
0x86 without CUEI, LATM, a CAT plus scrambled ES, a PSIP base PID, and an
unlisted PID, asserting the catalog, the stats rows, and an export
round trip of the carried streams. The MPEG-2 video arm also runs the TSDuck
round trip and an FFmpeg decode of the export in `test/ts`.

## Related

- [Live PAT/PMT changes](/quest/m1/ts-reliability/pmt-lifecycle.md) - a revised `stream_type` takes the route this quest declares
