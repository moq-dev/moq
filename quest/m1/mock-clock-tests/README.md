# [XL] Tests run on a mocked clock

## Goal

No test proves that nothing happened by waiting, sleeps to let things
settle, or asserts on elapsed real time. Each one runs on a paused,
simulated, or `Manual` clock, or awaits the event it means. About 300 tests
do one of these today, most of them behind a dozen shared helpers. Tests
that only wait for progress under a generous timeout, and tests on real
sockets, devices, or processes, stay on the real clock.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, from an audit of every
test's clock):

- ✅ **Scope: negative proofs, settle sleeps, and elapsed-time
  assertions.** Not chosen: only the first two; also the ~530 tests that
  only wait for progress under a timeout, which flake little and would cost
  a much larger migration.
- ✅ m1, while the guards that stop new ones (the runtime guards and the
  `REVIEW.md` rule) landed in m0.

Rules every child keeps:

- Fix the shared helper first, then its callers. A helper that resolves
  `undefined` when a timeout runs out becomes one that advances a mocked
  clock past the deadline, or awaits the event.
- Never replace one real wait with a shorter or longer one, or with a
  quiet window.
- Fix a test and its siblings in other languages together.
- Each JS child removes the bare `hostTime();` opt-outs in its area, so the
  `bun test` guard (`js/clock/src/preload.ts`) covers the converted tests. An
  opt-out with a trailing reason comment is real I/O and stays.
- Each Rust child removes the `kio::clock::host_time()` calls in the
  tests it converts, so `just rs test`'s host-timer guard covers them. A test
  over a real QUIC endpoint or ICE agent is exempt without one, since those
  transports allow host timers themselves.

The counts come from a heuristic scan on 2026-11-06; re-scan each area
before starting, since tests land and move.

## Required

- [js/net tests run on a mocked clock](/quest/m1/mock-clock-tests/js-net.md) - about 120 js/net tests stop waiting on real time, through their shared helpers
- [moq-relay's mesh tests run on a mocked clock](/quest/m1/mock-clock-tests/rs-relay.md) - the relay's multi-relay quiet windows and settle sleeps
