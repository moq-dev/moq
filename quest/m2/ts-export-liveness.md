# [S] moq export ts reports each elementary stream's access units and how long it has been quiet

## Goal

`Export::stats` returns `ts::Stats` with #3489's per-PID row for every
elementary stream the exporter writes: the access-unit count and the gap since
the last, on the output's own PCR. An operator can tell that one track stalled
at the exporter while PSI and the other PIDs kept flowing. The other fields of
`ts::Stats` stay zero at egress. `moq export ts` logs a line when a row stops
advancing.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **Liveness only, no ETSI counters at egress.** The media-aware lane
  regenerates PAT, PMT, PCR and CC, so an egress check grades our muxer, never
  the contribution feed. Sync, TEI, CRC, CC, PAT and PMT on bytes the muxer
  constructs fail only on a code defect, which the `test/ts` hard checks catch
  in CI, and `PAT_error` timed on the output's own PCR never fires on a whole
  stall because that clock freezes too. `PCR_accuracy_error` would be
  permanently in alarm: the exporter's PCR fails ±500 ns on every file sample
  and the downstream groomer's output passes on every one
  ([T33](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-33-gate2-preparation.md)).
  PCR accuracy and wire timing belong to the groomer's monitoring of its own
  output, outside this tree.
- **The runtime failure worth catching is a per-track stall.** #3533 stalled
  video and primary audio at the exporter while PSI and the other PIDs
  continued and the relay kept transmitting; only a per-PID liveness detector
  on a subscriber found it
  ([T27](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-27-liveness-detector.md)).
- **Same type as ingest.** `Export::stats` returns `ts::Stats`, so a
  dashboard reads one schema at both edges. Additive on main.
- **No window in-tree.** The raw exporter delivers by group, with 102
  simultaneous media-time gaps over 0.3 s in 39 s on every PID, and thresholds
  learned there differ from the groomed wire's for the same content (T27). A
  window means something only with its monitoring point named, so the
  consumer picks it.

Implementation:

- `rs/moq-mux/src/container/ts/export.rs`: count an access unit per PID when
  its PES is written, and the gap on the output PCR. Fix the stale "TR 101
  290 flags a gap over 40 ms" comment near `PCR_INTERVAL` (V1.4.1's limit is
  100 ms), and the matching 40 ms defaults and the "inserts no null packets,
  and paces PCR once per media frame" sentence in `test/ts/README.md`.
- `rs/moq-cli/src/subscribe.rs` logs a stopped row the way `publish.rs` does.
- Tests in `export_test.rs`: a healthy export advances every row; a track that
  stops mid-run grows its gap while PSI and the other PIDs continue, the
  #3533 shape.

## Required

- [#3489](/quest/m1/3489-ts-import-stream-liveness.md) - the per-PID row this reuses
