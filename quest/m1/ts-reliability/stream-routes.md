# [M] Every TS PID gets a declared route

## Goal

TS import gives every PID of the programs it imports, other than the null
PID 0x1FFF, one declared route: decoded, whole-PES `pesPacket`, `verbatim`
section, captured PSI/SI, or not carried. Null packets are stuffing, rebuilt
by export, and stay uncounted. A PID the semantic lane does not carry shows
in `ts::stats` with a count and one warning, so an operator sees what to
move to the raw lane. Multiplex-wide PIDs no program claims are counted when
the import reads every program (an SPTS, or `--program all`); a
`--program N` import of an MPTS reports only its own program. A known
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
- **Section-framed types outside 0x05 and CUEI-marked 0x86** are PES-parsed
  today. The ISO 13818-6 DSM-CC types (0x0A to 0x0D) ride sections, so every
  unit fails the start code and counts as `damaged`. Target: route each
  DSM-CC type 13818-6 confirms as section-framed through `ensure_section`.
- **0x86 without CUEI** drops with a warning and no count. Count it in
  `refused`, since it is ambiguous by design.
- **LATM/LOAS AAC (0x11)** already rides `pesPacket` with the `mpegts`
  section, though the `Import` docs and the `ensure_stream` comment say it is
  dropped. Fix the docs to match.
- **PIDs outside the PMT and the captured SI** (CAT on 0x0001 and the
  EMM/ECM PIDs it or a CA descriptor names, TSDT, ATSC PSIP on 0x1FFB and the
  PIDs its MGT signals, the DVB RST/DIT/SIT PIDs, and any PID no table lists)
  drop silently. Count each in a `ts::stats` row. ATSC PSIP and conditional access are outside the semantic
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
program's PMT arrives. A listed PMT that does not arrive within the 0.5 s of
program clock TR 101 290 allows (`PMT_error_2`, already counted) stops
holding the gate, so a stale or corrupt program never disables the count.

Under `--program all`, one parser reads the multiplex and `Programs` routes
each PID only to the programs whose PAT entry or PMT claims it
(`Programs::readers`), while `Programs::stats` merges their rows by PID, so a
per-program drop row would shadow the program that carries the PID. `Programs` owns the
unclaimed-PID rows instead: a PID is dropped only when no program's PMT and
no captured SI claims it, counted once at the multiplex rather than per
program. A `--program N` import of an MPTS reassembles only its own PMT, so
it cannot tell another program's PIDs from unclaimed ones; it counts only its
own program's PIDs and leaves multiplex-wide accounting to `--program all`.
When the PAT lists only the selected program, it counts as above. Chosen
over passively acquiring every PMT, which adds a reassembler per program to
an import that serves one.
Classifying a PID reads `Programs::readers`, the PID-indexed table of every
claimed PID kept as PMTs arrive: a PID no program reads is unclaimed. Extend
`rs/moq-mux/benches/ts_programs.rs` to sweep PID count beside program count
and bitrate, so any slope the accounting adds shows.

A dropped PID reuses `stats::Stream`, with no new field: a row keyed by its
PID in `Snapshot::streams`, an empty `track`, `Class::Data`, and `refused`
counting the PID's dropped packets, with one warning on the first. Widen the
`Snapshot::streams` and `refused` docs to say so, and report that as a
public API change in meaning. A PID keeps one row for its whole life,
whoever owns it: a PMT that claims a dropped PID, releases a carried one, or
claims it again moves that row between the carrying `Import` and the
dropped-PID owner (`Programs` under `--program all`) with every cumulative
counter intact, since counters never go backward. `track` and `class` follow
the current owner, empty and `Data` while dropped, and two rows never
coexist under one key. Coordinate the serialized shape with
the [MPEG-TS sections](/doc/concept/stats.md#mpeg-ts-sections) of a media report.

Rust: `rs/moq-mux` TS import (and export for MPEG-2 video `pesPacket`
packetization). JS: none. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): list MPEG-2 video among the
`pesPacket` examples and the DSM-CC types among the section-framed ones.

Tests: generated `fixture::Mux` inputs with MPEG-2 video, each DSM-CC type,
0x86 without CUEI, LATM, a PAT network entry with a NIT, a CAT plus
scrambled ES, a PSIP base PID, an unlisted PID, media before its first PMT,
a two-program MPTS under `--program all` whose second program's media
precedes its PMT, a PAT listing a program whose PMT never arrives, one
PID's full lifecycle (unlisted, added by a PMT revision, removed while its
packets continue, then added again) with its row's counters checked at each
step, and the same MPTS under `--program 1` with no rows for
program 2's PIDs, asserting the catalog, the
stats rows (the unlisted PID's row asserted field by field), and an export
round trip of the carried streams. The MPEG-2 video arm also runs the TSDuck
round trip and an FFmpeg decode of the export in `test/ts`.
