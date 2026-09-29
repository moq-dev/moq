# [M] moq import ts counts the TR 101 290 errors of the feed it receives

## Goal

An operator polling `moq import ts`, or `moq-srt` through the same importer,
reads cumulative counters for the TR 101 290 checks that grade a contribution
feed at ingest: `TS_sync_loss`, `Sync_byte_error`, `PAT_error`,
`Continuity_count_error`, `PMT_error`, `Transport_error`, `CRC_error` (on
PAT and PMT), `PCR_repetition_error`, `PCR_discontinuity_indicator_error` and `PTS_error`.
Counters only: ETSI's fixed limits, no configuration, no verdict, and nothing
changes in what is published.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **The checks live beside the container**, in a private module under
  `rs/moq-mux/src/container/ts` that takes 188-byte packets and keeps the
  counters. No new crate: nothing outside the tree consumes one. The importer
  already classifies sync, continuity and TEI for routing; routing reads the
  module's classification rather than keeping a second.
- **Counters are named for the ETSI check** in snake case
  (`continuity_count_error`, ...), cumulative for the importer's life like
  `resyncs`: stream-wide on `ts::Stats`, per PID on the PID's row. No
  last-event timestamp and no green/amber/red roll-up; the rate is what an
  operator alarms on, and the stats sample dates the event.
- **Limits are ETSI TR 101 290 V1.4.1's and fixed**: 0.5 s for PAT and PMT,
  loss of sync after two or more consecutive corrupted sync bytes and
  acquisition after five consecutive correct ones (ISO 13818-1 G.1), 100 ms
  for PCR repetition (the 40 ms limit left TS 101 154 in 2005), 0 to 100 ms
  for PCR discontinuity, 700 ms for PTS. The PMT names the PIDs, and every
  packet is checked, so there is no sampling to configure.
- **No `PID_error` field.** ETSI defines it as a PMT-listed PID with no
  packets for a user-specified period. The per-PID rows #3489 landed in
  #4502 (`units`, access units delivered, and `quiet`, the time since the
  last) cover it and are stricter: a dead video path behind a
  live mux still sends adaptation-field-only PCR packets on its PID, which a
  packet count reads as live
  ([T27](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-27-liveness-detector.md)).
  The rows keep #3489's names so no field claims the ETSI name for a different
  measurement, and the consumer picks the window, as #3489 decided. The docs
  map `PID_error` onto them.
- **`CRC_error` covers PAT and PMT only.** TR 101 290 names CAT, PAT, PMT,
  NIT, EIT, BAT, SDT and TOT, and its table 5.1b cuts that to PAT and PMT for
  systems with reduced SI, which a contribution feed is. The importer parses
  only PAT and PMT, and [TS PSI reassembly](/quest/m1/ts-psi-reassembly.md)
  already drops a bad-CRC section and counts it as `crc_error`; this quest
  adopts that field. SI captured verbatim (NIT, SDT, EIT, BAT, TOT) is not
  CRC-checked, and CAT is out with the scrambled services below.
- **Intervals run on the program clock** #4502 reads, accumulated step by
  step with a jump bound: one corrupt PCR taken as an absolute distance
  fabricated a 23,861 s outage on every PID in the campaign's detector (T27).
  A clock that stops freezes every interval; the consumer sees that case as
  every count standing still across two samples.
- **PCR and PTS are graded on values, and documented as such.** Repetition and
  discontinuity compare consecutive PCR values on the PCR PID, not arrival
  times: that grades the encoder's insertion, which is what ingest can speak
  for, while the network in front of the importer (SRT, UDP, a pipe) re-times
  arrival. The docs name the domain beside the counters, because a
  value-domain pass is not a wire pass
  ([evidence §3.2](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/docs/evidence.md)).
  - A signalled jump is legal: the interval across a packet carrying
    `discontinuity_indicator` on the PCR PID is dropped from both checks (ISO
    13818-1 2.4.3.4); the importer already reads it in `timebase_break`. The
    33-bit wrap is modular, not a jump.
  - `PCR_discontinuity_indicator_error` is a consecutive difference outside 0
    to 100 ms without the flag, backwards included. On values alone a run of
    missing PCRs and an unsignalled jump are the same difference, so one
    forward difference over 100 ms counts under both checks; the docs say so
    rather than guess which it was.
  - `PTS_error` is per elementary stream on the transport clock. Verbatim
    streams with no cadence (SCTE-35, DVB subtitles) are left ungraded rather
    than watched against an invented number.
- **`PCR_accuracy_error` is out.** ±500 ns is graded either against a
  constant-rate model of byte position, which holds only on a CBR feed, or
  against arrival on the wire, which is an analyser measurement
  ([T33](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-33-gate2-preparation.md)).
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
  a pair of packets, since a single packet never routes. Timing arms: a
  150 ms PCR gap counts one repetition and one discontinuity error, an 80 ms
  gap counts nothing, a signalled 500 ms jump counts nothing, a placed wrap
  counts nothing, and a 1 s PTS gap on one PID counts one there and nowhere
  else.

## Required

- [TS PSI reassembly](/quest/m1/ts-psi-reassembly.md) - the bad-CRC PAT or PMT the importer survives and counts as `crc_error`

## Related

- [SRT import stats](/quest/m1/srt-import-stats.md) - the gateway forwards the same `stats()`
- [TS health stats](/quest/m2/ts-health-stats.md) - where these counters are published
