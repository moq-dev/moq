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
- **The PAT's network PID entry** (`program_number` 0) is filtered out on
  import, and `Export::build_psi` writes a PAT with only the program, while
  the NIT itself passes through on 0x0010 as a snapshot track. Export lists
  program 0 on 0x0010 whenever the catalog carries a NIT snapshot, so the
  rebuilt PAT points a receiver at the NIT it carries.
- **TS-scrambled packets** on an elementary PID become PES parse damage.
  Count them in `refused` instead, since the semantic lane cannot carry them
  and the raw lane can.

A PID counts as dropped only once every program the current PAT lists has
delivered its PMT: packets on a PID before then are acquisition, as
`Import::decode` already treats them, so media that precedes its PMT on a
mid-stream join never shows as refused, including one program's media
arriving before its PMT while another program's PMT is already known. A PAT
that adds a program suspends the count for PIDs no PMT claims until that
program's PMT arrives.
Under `--program all`, every per-program `Import` reads the whole multiplex
and `Programs::stats` merges their rows by PID, so a per-program drop row
would shadow the program that carries the PID. `Programs` owns the
unclaimed-PID rows instead: a PID is dropped only when no program's PMT and
no captured SI claims it, counted once at the multiplex rather than per
program. A `--program N` import of an MPTS reassembles only its own PMT, so
it cannot tell another program's PIDs from unclaimed ones; it counts only its
own program's PIDs and leaves multiplex-wide accounting to `--program all`.
When the PAT lists only the selected program, it counts as above. Chosen
over passively acquiring every PMT, which adds a reassembler per program to
an import that serves one.
Classifying a PID looks it up in one map of every claimed PID, kept as
PMTs arrive, never by searching each program's PMT. A benchmark in
`rs/moq-mux/benches` feeds `Programs` a generated MPTS and sweeps program
count and PID count independently, against a baseline without the
accounting, so any slope it adds beyond the existing per-program feed shows.

Prefer the existing `stats::Stream` row and its `refused` count over a new
field; if a dropped PID needs a new `Class` or member, report the public API
change. Coordinate the serialized shape with
[TS health counters](/quest/m2/ts-health-stats.md).

Rust: `rs/moq-mux` TS import (and export for MPEG-2 video `pesPacket`
packetization). JS: none. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): list MPEG-2 video among the
`pesPacket` examples and the DSM-CC types among the section-framed ones.

Tests: generated `fixture::Mux` inputs with MPEG-2 video, each DSM-CC type,
0x86 without CUEI, LATM, a PAT network entry with a NIT, a CAT plus
scrambled ES, a PSIP base PID, an unlisted PID, media before its first PMT,
a two-program MPTS under `--program all` whose second program's media
precedes its PMT, and the same MPTS under `--program 1` with no rows for
program 2's PIDs, asserting the catalog, the
stats rows, and an export round trip of the carried streams. The MPEG-2 video arm also runs the TSDuck
round trip and an FFmpeg decode of the export in `test/ts`.

## Related

- [Live PAT/PMT changes](/quest/m1/ts-reliability/pmt-lifecycle.md) - a revised `stream_type` takes the route this quest declares
