# [S] Windows relay restart drill passes in the merge queue

## Goal

`moq-relay::drills relay_killed_mid_group_aborts_then_resumes` (both the
`loopback` and `impaired` lanes) passes on the Windows Platform job, so
Platform can become a required merge-queue gate.

## Plan

- It fails on every merge-queue Platform run that executes Windows, back to
  at least 2026-10-16 05:41 UTC (runs 38857400879 through 38931667318). Both
  lanes panic in `expect_status` at `rs/moq-relay/tests/drills.rs:661`:
  "no status change within 10s, wanted Connected". Linux and macOS pass.
- Find when it started (bisect the merge-queue history) and the root cause:
  why the reconnect loop never reports `Connected` after the relay restarts on
  Windows. Fix the cause; do not raise the timeout or skip the test on Windows.
- Land a regression check that fails without the fix on Windows. The drill
  itself is that check if it runs in the queue.
