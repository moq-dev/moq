# Tests run on a mocked clock

## Goal

No test proves that nothing happened by waiting, sleeps to let things
settle, or asserts on elapsed real time. Each one runs on a paused,
simulated, or `Manual` clock, or awaits the event it means. Tests that only
wait for progress under a generous timeout, and tests on real sockets,
devices, or processes, stay on the real clock.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, from an audit of every
test's clock):

- ✅ **Scope: negative proofs, settle sleeps, and elapsed-time
  assertions.** Not chosen: only the first two; also the ~530 tests that
  only wait for progress under a timeout, which flake little and would cost
  a much larger migration.
- ✅ m1, while the guards that stop new ones (the runtime guards and the
  `REVIEW.md` rule) landed in m0.

Rules the remaining work keeps:

- Fix the shared helper first, then its callers. A helper that resolves
  `undefined` when a timeout runs out becomes one that advances a mocked
  clock past the deadline, or awaits the event.
- Never replace one real wait with a shorter or longer one, or with a
  quiet window.
- Fix a test and its siblings in other languages together.
- An opt-out with a trailing reason comment (`hostTime(); // a real HTTP
  server`, `kio::clock::host_time(); // real SRT sockets`) is real I/O and
  stays. A bare one marks a test still to convert: remove it with the
  conversion, so the `bun test` guard (`js/clock/src/preload.ts`) or
  `just rs test`'s host-timer guard covers the test. A test over a real QUIC
  endpoint or ICE agent needs none, since those transports allow host timers
  themselves.

What remains, from a scan on 2026-11-07; re-scan before starting, since
tests land and move:

- **moq-relay's TCP-only tests**, the seven bare `kio::clock::host_time()`
  calls in `tests/integration/auth_lifetime.rs` (4), `session_revalidate.rs`
  (2), and `socket_auth.rs` (1). They wait out real re-check cadences and the
  handshake deadline over TCP, bound a close by a real budget ("well inside
  the cadence"), and poll listings on kio sleeps. The relay runs on the test
  runtime, so an installed `kio::clock::Manual` can own its timers while the
  sockets stay real: a push then proves its close with the clock standing
  still, and a cadence passes by `advance`. `wait_listed` and `assert_closed`
  need awaited events first. `socket_auth`'s test waits only for progress,
  so find which relay timer fires there.
- **The OBS plugin tests** in `cpp/obs/test`, which neither runtime guard
  sees. `moq-output-test.cpp` and `moq-source-test.cpp` sleep 50 to 100 ms,
  then check that no further signal or frame arrived; `obs-moq-test.cpp`
  proves a parked `accept` by waiting 50 ms; `moq-dock-stop-test.cpp`
  asserts `20ms <= elapsed < 400ms` around a worker that sleeps 30 ms. Each
  needs an event from the plugin, such as its worker joining or the bridge
  observing the close, instead of a window.

## Required

- [A lint rejects expect(clock.drive(...)).rejects and .resolves](/quest/m1/mock-clock-tests/drive-expect-guard.md) - wrapping `drive` in `expect(...).rejects` or `.resolves` fails lint
