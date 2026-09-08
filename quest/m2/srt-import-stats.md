# [S] moq-srt reports the same import stats as moq import ts

## Goal

An operator running the SRT gateway sees the TS importer's per-stream
counters (access units, quiet gap, resyncs) the way `moq import ts` logs
them, instead of nothing. SRT contribution is where a silent PID matters
most, and today `rs/moq-srt/src/ts.rs` only calls `decode`.

## Plan

- Poll `self.importer.stats()` on the SRT publisher's existing
  `ts::Import<ts::Ext>` at the same cadence as the CLI.
- Share the logging presentation with `rs/moq-cli/src/publish.rs` where
  needed so the two front doors report the same rows.
- Test: the SRT harness feeds the suppressed-PID stimulus from the TS
  liveness quest and asserts the gateway reports the stalled row.

## Required

- [#3489](/quest/m2/3489-ts-import-stream-liveness.md) - the rows this forwards
