# Quarantined tests hold up under load

## Goal

Tests that pass alone but fail under a loaded `just check`, or intermittently
in CI or interop, pass reliably, each fixed at its cause, never by raising a
timeout or adding a retry.

## Plan

Continues the finished test flakes epic (deleted by
[moq-dev/moq#9573](https://github.com/moq-dev/moq/pull/9573); see its history
for earlier rounds). Decided 2026-11-15 (maintainer): a new flake joins this
epic while it is open; once it completes, the next flake opens a fresh one.
One child per flake, grouping only those that share a fixture.

Rules every child keeps:

- Prefer a paused clock over wall time, or assert on an event instead of a
  deadline. If a test is slow under load because the code under test is slow,
  fix that.
- Where real sockets fight the paused clock, drive the timers with a
  `kio::clock::Manual` clock or the sim, or move the test off real sockets.
- Where the code jitters its timers with `rand`, wait out the jitter's whole
  window, not a count of small steps.
- Reproduce before fixing; if the flake no longer reproduces on `main`, find
  the PR that fixed it and delete the child.

Public API: none. Wire: none.
