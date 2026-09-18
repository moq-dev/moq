# [S] moq ts: record the source mux rate and pad export to it

## Goal

`moq import ts` records the transport stream's constant rate in the catalog,
and `moq export ts` pads its output with null packets to that rate, so a
broadcast that came in as a CBR multiplex leaves as one an IRD or groomer can
accept without measuring anything. The rate is the whole multiplex, every PID
plus PSI plus stuffing, which is what PCR clocks; per-rendition `bitrate`
keeps its codec meaning and is not touched. Import of a VBR or unpaced source
records nothing, and export of a catalog without the field is byte-identical to
today.

Non-goals: MSFTS catalog convergence, carrying source PCR, SI coverage, and
group alignment stay in [#3731](https://github.com/moq-dev/moq/issues/3731)
until the next MSFTS revision is published; this quest settles only decision 6
there.

## Plan

- `rs/moq-mux/src/container/ts/catalog.rs`: add `mux_rate: Option<u64>` to
  `Mpegts` (serialized `muxRate`, bits per second) and include it in
  `is_empty`. Document it as the rate the PCR clock paces the multiplex at,
  not a sum of elementary streams.
- Import (`rs/moq-mux/src/container/ts/import.rs`): the PCR PID already parses
  its adaptation field. Between two PCRs on that PID, the rate is
  `packets * 188 * 8 / (delta PCR / 27 MHz)`; count every packet of the stream
  including null PID `0x1fff`, which the routing gate currently drops before
  counting. Publish the rate once it is stable across a window (a few seconds
  of PCR intervals within a small tolerance) and republish the catalog when it
  changes materially; leave it absent when intervals disagree, which is what
  a VBR or file-paced source looks like. Reset on a PCR discontinuity.
- Export (`rs/moq-mux/src/container/ts/export.rs`): the exporter already emits
  one `Frame` per 25 ms PCR grid slot. When `mux_rate` is present, each slot
  owes `mux_rate * 25 ms / (188 * 8)` packets; after the slot's media, PSI, and
  SI packets, fill the remainder with null packets (PID `0x1fff`, no
  adaptation, payload of `0xff`). A slot that already exceeds its budget emits
  no nulls and carries the debt forward so the long-run rate holds. Pad only
  when the field is present; `moq export ts --mux-rate <bps>` overrides or
  supplies it for a catalog without one.
- Docs: `doc/bin/cli.md` gains the flag and a sentence on padding;
  `doc/draft/moq-hang.md` describes the field if the `mpegts` section is
  documented there, otherwise the crate docs carry it.
- Tests: an import fixture with a known CBR rate and stuffing (the existing
  `test_data` sources, or a synthesized one) yields the expected `muxRate`
  within tolerance; a VBR fixture yields none; export with the field emits a
  stream whose measured rate matches and whose PCR intervals stay under 40 ms;
  export without it is unchanged.

Public API: one additive field on the `Mpegts` catalog section and one CLI
flag. Wire: an additive catalog field; no draft change, the `mpegts` section
is an extension.

## Closes

- [#3731](https://github.com/moq-dev/moq/issues/3731) - close this issue when the quest finishes

## Related

- [#2779](/quest/m2/2779-moq-export-ts-continuity-counters-are-numbered-from.md) - the other determinism gap in the same exporter
- [TR 101 290](/quest/m3/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - the monitoring that would grade the padded output
