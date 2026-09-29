# [M] Per-program SI

## Goal

When an MPEG-TS import selects a program (`ts::Import::with_program`, so
`moq import ts --program <n>` and each broadcast of `--program all`), that
program's broadcast carries SI describing only its own service. Other services'
EIT sub-tables are dropped, and the SDT for this transport stream is rewritten
to list only the selected service. Network-wide tables (NIT, BAT, TDT/TOT, SDT
other, EIT other) pass through unchanged. An import without a selection keeps
capturing every section byte-for-byte, as today.

## Plan

Settled decisions:

- Filter at import, in `si::Capture`, before a generation commits, so a
  program's SI tracks never carry another service's bytes and exporters stay
  unchanged. Filtering at export was rejected: every subscriber would still
  receive the whole multiplex's schedule.
- Filter only under an explicit selection. A single-program input whose SDT
  lists extra services is left alone, because default capture promises verbatim
  sections.
- EIT actual (`table_id` 0x4E and 0x50..=0x5F): keep a sub-table only when its
  `table_id_extension` (the service_id) is the selected program_number. Whole
  sub-tables are dropped; nothing is rewritten. EIT other (0x4F and
  0x60..=0x6F) describes other transport streams and passes through.
- SDT actual (`table_id` 0x42 on PID 0x11): its one sub-table lists every
  service, so once a generation is complete, rebuild it as a single section
  (`section_number` and `last_section_number` 0) holding the selected service's
  loop entry, wherever it sat in the source's sections. Keep the header fields
  (transport_stream_id, version_number, original_network_id), recompute
  `section_length`, and write a fresh CRC-32/MPEG-2 using the `crc` crate, which
  is already in `Cargo.lock`, as a direct moq-mux dependency rather than a
  hand-written table.
- When the SDT has no entry for the selected service, carry no SDT actual rather
  than fabricating a table the source never gave for this service. A later
  version that lists it is captured normally.
- A `(PID, table_id)` whose every sub-table is filtered away gets no catalog
  `mpegts.si` entry or track.
- The same change updates the program-selection paragraph in `doc/bin/cli.md`
  and the `si.rs` module docs; no new page is needed.

Test with a synthetic two-service multiplex whose SDT lists both services
across two sections, with EIT present/following for each. Each selected import
carries one SDT entry with a valid CRC and only its own EIT; an unselected
import keeps the sections verbatim; a selection missing from the SDT carries
none; and the re-exported TS still parses.

## Required

- Program selection ([#4505](https://github.com/moq-dev/moq/pull/4505)) is on main
