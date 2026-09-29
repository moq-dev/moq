# [S] moq import ts drops a PAT or PMT section it cannot parse instead of ending

## Goal

A contribution feed that delivers one corrupted PAT or PMT section keeps
publishing: the importer drops the section, counts a CRC mismatch as TR 101
290 `CRC_error`, and keeps the layout it already had until the next good
repetition. A feed that never delivers a good PAT and PMT still publishes
nothing, as today.

## Plan

Reproduced on main: a PAT with one CRC byte flipped, between two good PAT/PMT
pairs, makes `Import::decode` return the `mpeg2ts` PSI reader's
`CRC32 mismatch`, and `rs/moq-cli/src/publish.rs` ends the publish on it. A
malformed adaptation field on the PAT PID ends it the same way. The PMT goes
through the same reader. Line noise on a live feed should cost one repetition,
0.5 s at most, not the broadcast; a receiver discards the section and waits
for the next.

- `rs/moq-mux/src/container/ts/import.rs`: the `read_ts_packet()?` loop
  treats a parse failure on the PAT or a PMT PID as a dropped section, the way
  `Continuation::Corrupt` already drops a packet the demodulator disowned.
  Reader errors on other PIDs keep their current handling. The section is
  refused whole, never half-applied, so fail-loud holds at section
  granularity.
- `ts::Stats` gains a cumulative `crc_error`, named for the ETSI check like
  the counters in [TS import health](/quest/m2/ts-import-health.md), which
  counts the other parse failures under `PAT_error` and `PMT_error`. Additive
  on main: `Stats` is `#[non_exhaustive]`.
- Tests: the stimulus above on the PAT and on the PMT, asserting decode
  succeeds, the layout survives, the counter reads one, and a later good PMT
  revision still applies. A positive control: a feed whose only PAT is
  corrupt publishes nothing and counts it.

## Related

- [TS import health](/quest/m2/ts-import-health.md) - the rest of the ingest counters
