# [L] TS PSI reassembly

## Goal

The MPEG-TS importer reads a PAT or PMT whose section spans several packets or
starts after a nonzero `pointer_field`, instead of aborting the import. Today
the `mpeg2ts` 0.6.1 reader parses PSI from a single packet and rejects a nonzero
`pointer_field`, and its error ends the whole import, so a valid long PMT (many
audio languages, long descriptors) or a PAT with more than about 40 programs
cannot be imported. `ts::programs()` finds such a PAT too.

## Plan

Settled decisions:

- moq-mux owns the packet demux, and the import stops using
  `mpeg2ts::ts::TsPacketReader`. It can't be fixed from outside: the reader's
  PAT and PMT parsers are private to the crate, and it learns PES PIDs only
  from PMTs it parsed itself. Feeding it a synthesized one-packet PMT to
  register PIDs was rejected as fragile coupling, and fixing it upstream in
  sile/mpeg2ts was rejected as slower and outside this repository. Switching to
  `mpeg2ts-reader` was rejected too: it is a callback-driven demux framework
  (last release January 2025), and moq-mux already owns the hard part in
  `SectionReassembler`. Owning the demux also deletes the importer's
  workarounds for the reader: the `Feed` mutex it reads through, and the
  `pmt_pids`/`streams` gate that keeps unknown PIDs away from it. `mpeg2ts`
  stays for export, tests, and plain types such as `StreamType`.
- PID 0 and every PMT PID go through the existing `SectionReassembler`, which
  already handles `pointer_field`, split headers, and continuity gaps. The
  PAT (`transport_stream_id`, version, program loop, possibly several sections)
  and the PMT (program number, PCR PID, program descriptors, and the
  elementary-stream loop with descriptors) are parsed in moq-mux. PES is
  routed by the PIDs of the parsed PMT, and its header (PTS/DTS) is also parsed
  in moq-mux.
- Each reassembled section's CRC-32/MPEG-2 is checked with the `crc` crate.
  [Per-program SI](/quest/m2/ts-program-si.md) needs the same dependency, and
  whichever change lands first adds it. A section with a bad CRC is dropped and
  the last good table stays in force, so the import continues; it no longer
  aborts as the `mpeg2ts` reader does. The section is refused whole, never
  half-applied, so fail-loud holds at section granularity. A malformed
  adaptation field on the PAT or a PMT PID, which ends the import today too,
  costs that section the same way. A feed that never
  delivers a good PAT and PMT still publishes nothing. Line noise on a live
  feed then costs one repetition (0.5 s at most), not the broadcast; today one
  flipped CRC byte on `bbb.ts` makes `decode` return `CRC32 mismatch` and
  `moq import ts` exit.
- Each dropped section is counted in `Import::stats` as a cumulative
  `crc_error`, so a corrupt feed is visible rather than silently held on a
  stale table. It is stream-wide on `Stats`, since the PAT and PMT PIDs have no
  elementary-stream row, and named for the TR 101 290 check that
  [TS import health](/quest/m2/ts-import-health.md) adopts; a section that
  fails to parse for another reason is left to that quest's `PAT_error` and
  `PMT_error`. `Stats` is `#[non_exhaustive]`, so the new field is additive;
  its docs (today per elementary stream) and `is_empty` widen to cover it, and
  `moq-cli`'s `log_stats` reports it.
- `ts::programs()` reads through the same PAT path, so a PAT spanning packets
  is found before any program publishes.
- One quest, because the demux refactor alone changes nothing observable.

Keep every existing TS import test and fixture passing unchanged. Add tests for
a PMT spanning two packets, a PAT behind a nonzero `pointer_field`, a
multi-packet PAT with enough programs to need it (read by `ts::programs()` and
by `with_program`), and a PAT and a PMT with a corrupt CRC, each between good
repetitions, that is dropped and counted once while the import keeps its
previous layout and a later good PMT revision still applies. A positive
control: a feed whose only PAT is corrupt publishes nothing and counts it.

Folded in from `ts-import-psi-crc` while landing the TR 101 290 plan (#4496):
that quest duplicated this one's bad-CRC drop and `CRC_error` count.

## Required

- [moq import ts: select programs](/quest/m1/ts-programs.md) - `ts::programs()` and `with_program`, which this quest's PAT path and tests build on
