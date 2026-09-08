# [S] moq-srt reports the same import stats as moq import ts

## Goal

An operator running the SRT gateway sees the TS importer's per-stream
counters (access units, quiet gap, resyncs) the way `moq import ts` logs
them, instead of nothing. SRT contribution is where a silent PID matters
most, and today `rs/moq-srt/src/ts.rs` only calls `decode`.

## Plan

- Add a `stats` forwarder to `ContainerImpl` in
  `rs/moq-mux/src/import/container.rs`, so every container importer exposes
  its stats through one shape rather than the TS-specific enum arm
  `rs/moq-cli/src/publish.rs` carries today. Containers without counters
  return an empty report, not `None`.
- `rs/moq-srt` polls it on the same cadence as the CLI and logs the same
  lines, so the two front doors read alike.
- Test: the SRT harness feeds the suppressed-PID stimulus from the TS
  liveness quest and asserts the gateway reports the stalled row.

## Required

- [#3489](/quest/m2/3489-ts-import-stream-liveness.md) - the rows this forwards
