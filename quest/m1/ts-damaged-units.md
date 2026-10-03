# [M] TS import drops a damaged unit instead of ending the ingest

## Goal

`moq import ts`, and `moq-srt` through the same importer, survive a packet,
PES, or access unit they cannot parse: a malformed PES header or media-PID
adaptation field with `transport_error_indicator` clear, or a codec error such
as an H.264 NAL with `forbidden_zero_bit` set. The damaged unit is dropped and
counted, its track waits for its next keyframe, and the ingest carries on.
Today the first such error ends the import, and the gateway hangs up.

## Plan

Today `decode` in `rs/moq-mux/src/container/ts/import.rs` runs
`while let Some(packet) = self.reader.read_ts_packet()? { self.handle_packet(packet)?; }`,
so any PES-header error from the reader and any codec error from `flush` ends
the import. The `transport_error_indicator` branch already drops a flagged
packet (clears `pending[pid]` and calls `stream.desync()`), and it is the model.

Decided (maintainer, 2026-09-30):

- Fail-loud holds at unit granularity, as the importer
  holds it at section granularity for PSI (#4584): a damaged unit is refused whole, never half
  published, and counted, and nothing else about the feed changes. PSI damage
  is already handled.
- Drop the unit the way the TEI branch does: clear the PID's pending PES and
  desync its stream, so a codec that needs one waits for the next keyframe.
  Make sure the scratch buffer still advances past the packet on this path
  (today an error skips `self.scratch.drain(..off)`).
- Count each drop in a new cumulative per-PID `damaged` counter on the stream's
  stats row, beside `resyncs` and `discarded`, reported by `ts::stats::Log`.
  It names what happened; no TR 101 290 check matches a codec error, and
  [TS import health](/quest/m2/ts-import-health.md) keeps the ETSI names.
  The rows are `#[non_exhaustive]`, so the field is additive; follow
  [TS stats module](/quest/m1/ts-stats-module.md)'s names if it has landed.
- Errors that are not confined to one unit (the producer refusing a rewind,
  origin or catalog failures) stay fatal.
- Built on the demux TS PSI reassembly owns, since that quest moves PES header
  parsing into moq-mux.

Tests, from [#4581](https://github.com/moq-dev/moq/issues/4581)'s stimuli on a
fixture: a video PES header with its flag and timestamp bytes zeroed (TEI
clear), and one H.264 NAL with `forbidden_zero_bit` set. Each import carries
on, counts exactly one `damaged` on that PID and none elsewhere, and publishes
again from the next keyframe; a clean fixture counts zero. Document the counter
wherever the TS stats fields are described.

Public API: additive, one stats field. Wire: none.

## Closes

- [#4581](https://github.com/moq-dev/moq/issues/4581) - one malformed packet ends the TS import

## Related

- [TS import health](/quest/m2/ts-import-health.md) - the TR 101 290 counters for the same feed
