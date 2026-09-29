# [M] moq export ts checks the TS it emits

## Goal

`Export::stats` reports the ingest counters over the exporter's own output,
for the checks that can fail at runtime in a stream our muxer builds:
`PAT_error`, `PMT_error`, `Continuity_count_error`, `PID_error`, `PTS_error`,
and `PCR_repetition_error` and `PCR_discontinuity_indicator_error` on PCR
values. These grade our muxer, not the contribution feed: the media-aware lane
regenerates PAT, PMT, PCR and CC, so no egress figure speaks for the source.
`moq export ts` logs a line when one moves.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **The shared module reads the emitted bytes**, not the muxer's
  bookkeeping, so a bookkeeping bug is caught rather than agreed with. The
  cost is a header parse per emitted packet; null stuffing is skipped.
- **Runtime failures are the point.** #3533 stalled video and primary audio
  at the exporter while PSI and the other PIDs continued and the relay kept
  transmitting; a per-PID liveness detector on a subscriber found it, and
  neither P1/P2 conformance nor the relay's counters showed it
  ([T27](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-27-liveness-detector.md)).
  Intervals run on the output's own PCR, as at ingest, so an export that
  stalls whole freezes its clock and shows as every count standing still
  across two samples.
- **`PID_error` is a gap gauge** per PMT-listed PID, counting PES starts
  (PUSI, a payload, `00 00 01`) as the ingest row counts access units. No
  window in-tree: the raw exporter delivers by group, with 102 simultaneous
  media-time gaps over 0.3 s in 39 s on every PID, and thresholds learned
  there (1.0 to 1.95 s) differ from the groomed wire's (1.0 to 2.94 s) for the
  same content (T27). A window means something only with its monitoring point
  named.
- **PCR on values only.** The exporter's PCR sits on an exact 25 ms grid
  while media flows (file, P1,
  [T33](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-33-gate2-preparation.md)),
  so the value checks fire only on an outage past `PCR_BACKFILL` or a
  timebase jump that leaves without `discontinuity_indicator`.
- **Not applicable at egress**, and not counted:
  - `TS_sync_loss`, `Sync_byte_error`, `Transport_error` and `CRC_error`
    grade bytes the muxer constructs; a failure is a code defect that the
    `test/ts` hard checks catch in CI, not a runtime condition.
  - `PCR_accuracy_error` would be permanently in alarm. On file the
    exporter's PCR fails ±500 ns on every sample, 790 of 790, and the
    groomer's output passes on every sample, 0 of 927 (T33): the exporter
    does not emit a constant-rate wire, and building one is the downstream
    groomer's job.
  - Wire-domain PCR repetition is invisible in-process, and what a receiver
    sees is the groomer's re-stamped PCR: after grooming, 0 of 20,193
    intervals above 40 ms over 300 s on the wire
    ([evidence §3.2](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/docs/evidence.md)).
  - PCR accuracy and wire timing belong to the groomer's monitoring of its
    own output
    ([architecture §4](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/docs/architecture.md)),
    outside this tree.

Implementation:

- `rs/moq-mux/src/container/ts/export.rs`: `Export::stats` returns
  `ts::Stats`, the ingest type, with the fields that do not apply left zero.
  Additive on main.
- `rs/moq-cli/src/subscribe.rs` logs moves the way `publish.rs` does.
- Tests in `export_test.rs`: a healthy export reads zero on every counter; a
  track that stops mid-run grows its PID gap while PSI and the other PIDs
  continue, the #3533 shape; a payload-less PCR packet does not advance CC.

## Required

- [TS import health](/quest/m2/ts-import-health.md) - the module and `ts::Stats` fields this reuses
- [TS import timing](/quest/m2/ts-import-timing.md) - the value-domain PCR and PTS checks this runs on the output
