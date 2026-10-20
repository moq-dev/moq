# [XS] moq-uring's host clock test stops racing wall time

## Goal

`clock::tests::host_tracks_moq_clock` (`rs/moq-uring/src/clock.rs`) passes
under load. It calibrates `Host` and compares it with `moq_clock::now()`
within 1 ms of wall-clock slack, which a preempted thread exceeds.

## Plan

Seen during the 2026-10-20 merge batch.

- Per the epic's rules, never widen the slack. Test the mapping, not the
  scheduler: calibrate from a known `(Instant, monotonic)` pair and assert
  that `now()` and `monotonic(at)` translate between the two origins exactly,
  so nothing depends on how long the thread waits between reads.
- If the calibration itself can be off by a preemption between its two
  reads, that error reaches every deadline moq-uring computes; bound it in the
  code (read until two samples agree, or take the narrower bracket) rather
  than in the test.

Public API: none. Wire: none.
