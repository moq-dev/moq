# [M] moq's sim suites run under adversarial schedules

## Goal

moq's tests that run in `kio::clock::sim` also run under `KIO_SIM_SCHEDULE`
(`random` and `pct`) with `KIO_SIM_PROBE=1`, the lost-wakeup probe. Each
finding is fixed at its root or planned as a quest.

## Plan

Decided 2026-11-27 (maintainer, /quest-plan on kio's follow-ups).

- ✅ m4 (rejected: drop it).
- ✅ The sweep runs on every PR if each pass stays within a few seconds,
  otherwise post-merge, with the measured cost in the PR (rejected: always
  post-merge).
- Pattern: `just rs sweep`, which Nightly runs over kio's own suite, and the
  module docs in `rs/kio/src/clock/sim.rs`
  ([kixel-ai/kio#10](https://github.com/kixel-ai/kio/pull/10)). Widen that
  recipe rather than adding another.
- Scope: 107 files name `kio::clock::sim` (moq-net 77, moq-relay 10,
  moq-tokio 9, moq-net-tests 5, a few others), and `#[kio::test]` runs every
  test it marks in the sim too, so the sweep covers far more than those files.
- From kio#26's report: the probe counts progress only through kio state (a
  channel, a wake), and it rejects wakes from other threads, so a test that
  hands work to a host thread fails the probe rather than finding a bug. Keep
  such tests on one thread.

Public API: none. Wire: none.
