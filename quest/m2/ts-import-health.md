# [M] moq import ts counts the TR 101 290 priority 1 errors of the feed it receives

## Goal

An operator polling `moq import ts`, or `moq-srt` through the same importer,
reads cumulative counters for the TR 101 290 checks that grade a contribution
feed at ingest: `TS_sync_loss`, `Sync_byte_error`, `PAT_error`,
`Continuity_count_error`, `PMT_error`, `Transport_error` and `CRC_error`, with
`PID_error` read from the per-PID liveness rows. Counters only: ETSI's fixed
limits, no configuration, no verdict, and nothing changes in what is
published.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **The checks live beside the container**, in a private module under
  `rs/moq-mux/src/container/ts` that takes 188-byte packets and keeps the
  counters. The importer feeds it its input and the exporter its output
  ([TS export health](/quest/m2/ts-export-health.md)), so each check has one
  definition. No new crate: nothing outside the tree consumes one. The
  importer already classifies sync, continuity and TEI for routing; routing
  reads the module's classification rather than keeping a second.
- **Counters are named for the ETSI check** in snake case
  (`continuity_count_error`, ...), cumulative for the importer's life like
  `resyncs`: stream-wide on `ts::Stats`, per PID on the PID's row. The rate
  is what an operator alarms on.
- **Limits are ETSI's and fixed**: 0.5 s for PAT and PMT, loss of sync after
  five corrupted sync bytes and regain after two. The PMT names the PIDs, and
  every packet is checked, so there is no sampling to configure.
- **Intervals run on the transport clock** #3489 parses, accumulated step by
  step with a jump bound: one corrupt PCR taken as an absolute distance
  fabricated a 23,861 s outage on every PID in the campaign's detector
  ([T27](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-27-liveness-detector.md)).
  A clock that stops freezes every interval; the consumer sees that case as
  every count standing still across two samples.
- **`PID_error` is #3489's row**: access units and the gap since the last,
  per elementary stream, with the ETSI window left to the consumer as #3489
  decided. It counts access units, not packets, on purpose: a dead video path
  behind a live mux still sends adaptation-field-only PCR packets on its PID,
  which a packet count reads as live (T27).
- **No opaque whole-mux lane**:
  [#1861](https://github.com/moq-dev/moq/issues/1861) is closed not planned
  and verbatim TS is a non-goal in
  [MSFTS convergence](/quest/m4/msfts-convergence.md). P3 is out of this set;
  `CAT_error` too, since the lane carries no scrambled service. A scrambled
  PAT or PMT still counts under its own check.

Implementation:

- `rs/moq-mux/src/container/ts/import.rs`: `Stats` and `StreamStats` are
  `#[non_exhaustive]`, so the fields are additive. A continuity break
  declared by `discontinuity_indicator`, the one duplicate ISO 13818-1
  2.4.3.3 permits, and a payload-less packet repeating its counter are not
  errors.
- `rs/moq-cli/src/publish.rs` `log_stats` logs a line when a counter moves,
  as it does for resyncs.
- Measure `decode` throughput on `test_data/kyrion_mpeg2av_ac3.ts` before and
  after; the checks read header bytes the loop already reads, so a measurable
  cost is a finding.
- Tests, one stimulus per check with a control each way: a clean capture
  reads zero, the faulted one reads exactly the count injected. Feed at least
  a pair of packets, since a single packet never routes.

## Required

- [TS import PSI CRC](/quest/m2/ts-import-psi-crc.md) - `CRC_error` needs a CRC failure the importer survives
- [#3489](/quest/m1/3489-ts-import-stream-liveness.md) - the transport clock and the per-PID rows this adopts as `PID_error`

## Related

- [SRT import stats](/quest/m1/srt-import-stats.md) - the gateway forwards the same `stats()`
- [TS health stats](/quest/m2/ts-health-stats.md) - where these counters are published
