# [S] TS export writes a PMT that spans several packets

## Goal

`moq export ts` carries a programme whose PMT section is longer than one TS
packet (multi-language audio easily is), splitting it over consecutive
packets on the PMT PID instead of exiting before its first packet. A section
past the 1,024-byte limit (ISO 13818-1 `section_length` ≤ 1,021) fails the
export loudly.

## Plan

Decided (2026-10-10), importing #5110. The export hands mpeg2ts a
`TsPayload::Pmt`, which serialises the table into a single packet's payload
and fails with "failed to write whole buffer" once it overflows. mpeg2ts keeps
its section writer `pub(super)`, so it cannot be reused.

- Build the PAT and PMT sections ourselves in
  `rs/moq-mux/src/container/ts/psi.rs`, which already has the CRC and the
  parser, and packetise both through the export's existing `write_section`
  (pointer_field and unit start on the first packet, continuity counters
  running). That leaves a single PSI write path, so mpeg2ts no longer writes
  tables. Chosen over keeping the PAT on mpeg2ts (two paths) and over an
  upstream mpeg2ts change (an outside party).
- Test: a synthetic export with enough audio tracks carrying language
  descriptors to push the PMT past one packet. Assert that the output parses
  back to the same PMT, that the continuity counters run, and that a PMT over
  the limit fails loud. No external fixture.

## Closes

- [#5110](https://github.com/moq-dev/moq/issues/5110) - `export ts` exits before its first packet when the PMT is longer than one TS packet
