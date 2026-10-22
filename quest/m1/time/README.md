# [XS] Controlled time

## Goal

Every MoQ crate and package reads time and arms timers only through
`moq-clock` (Rust) or `@moq/clock` (JS), with the production and public API migrations completed in m1. Tests
needed to prove these changes use controlled time. The backend is
the current runtime's (tokio, browser, io_uring, or the simulator), so the
same logic runs on each and a test can swap in simulated time.

Tests that do real I/O (sockets, browsers, codecs, hardware) follow one rule:
behavioral assertions use controlled time; real-I/O tests synchronize on
events, with wall time allowed only for harness watchdogs and explicit
performance measurements.

Out of scope here: a runtime trait for spawn and I/O
([Runtime trait](/quest/m2/runtime-trait.md)), QUIC over simulated UDP
([QUIC on the sim](/quest/m2/quic-sim.md)), and seeded randomness
([Seeded sim](/quest/m2/sim-seed.md)).

## Plan

Decided 2026-10-10 from an audit of `main` (`355f45864`), with
reasons, so later sessions don't reopen them:

- **Why.** Flaky tests, and runtime independence: browser wasm has no
  `std::time::Instant`, and paused tokio time does not pause
  `std::time::Instant`, so a test that mixes them (std `elapsed()` beside a
  tokio sleep) advances one clock and not the other.
- **One clock supplies both now and deadlines.** Wrapping `Instant` alone does
  not make tests deterministic.
- **A new published crate, `moq-clock`,** extracted from moq-net's private
  `Clock`/`Deadline`, not a public `moq_net::time` (mux, downstream apps, and other
  crates would depend on moq-net for time) and not web-async (a separate repo,
  and the thing the runtime trait replaces).
- **Named `moq-clock`,** with its proc-macro helper `moq-clock-macros`
  (decided 2026-10-13 by the maintainer, replacing `moq-time`). The crates.io
  name is ours: the "CLOCK over QUIC" demo binary held it until 0.10.21 and
  stopped publishing when #1488 and #1494 made it the moq-tokio `clock`
  example. The library publishes as 0.11.0, a semver break from the demo.
  The JS mirror is `@moq/clock` (same decision), and the private JS CLOCK
  demo that held that name is gone. Wall time is `moq_clock::Wall`, so it
  never reads as `moq_net::Timestamp` (media time).
- **The instant is logical:** `Instant(u64)` nanoseconds from the clock's
  origin, so a test starts at zero with fixed coordinates and never reads the
  host clock. Native, wasm, and io_uring instants convert at the backend.
  Nanoseconds match `Duration` and QUIC pacing. In JS a plain `number` of
  nanoseconds is exact for 104 days of uptime and only loses sub-microsecond
  precision after. The browser backend must preserve clock origin and units
  across Rust and JS; a source translator's `Instant` mapping is chosen with
  that backend, while WASM retains Rust time types. Its methods mirror
  `std::time::Instant`'s, so swapping one for the other is an import change.
- **Wall time is a separate type from the same clock.** The manual clock's
  `advance()` moves both; `step_wall()` moves only wall time, so a test can
  prove a clock correction never manufactures elapsed (billable) time.
- **Runtime-ambient access.** State machines keep taking `now` (moq-net's
  `Driver::poll(now, waiter)`). Async glue
  calls `moq_clock::now()`, `sleep()`, `timeout()`, and `interval()`, which
  resolve to the current runtime's clock: one clock per runtime, nothing to
  plumb, no process-global test override. The ambient functions stay behind
  moq-net's `async` feature, so the sans-IO core keeps explicit instants.
- **The simulator folds into `moq-clock`** behind a `sim` feature (a
  dev-dependency only, so production builds never compile it). It is one
  backend of the ambient clock, starts at zero, keeps deadlock detection, and
  seeds the later runtime trait. `#[moq_clock::test]` runs tests on this
  backend.
- **Tests for migrated contracts use the sim wherever possible.** A test with no real I/O
  that cannot run on the sim yet uses paused tokio, whose clock `moq-clock`
  reads. A real-I/O test never uses paused tokio, which auto-advances to the
  next timer while the OS event is still in flight; it syncs on events, with
  wall time only for its watchdog.
- **Landing.** Everything lands on `main`, the trunk. The crate and the JS
  package land first and additively; the migrations that change a published
  type (moq-net's `time::Instant`, moq-mux's `Clock`, `@moq/net`'s time
  units) follow as breaking changes. No backport: `release` is touched only
  to cut a release or backport a critical fix, so pinned consumers adopt
  `moq-clock` from the next cut (decided 2026-10-10 from review, replacing an
  earlier backport plan).
- **Lint ratchet.** Each migration quest bans direct clock reads and timers in
  the crates and packages it finishes: clippy `disallowed-methods` and
  `disallowed-types` in a per-crate `clippy.toml`, and a biome
  `noRestrictedGlobals` override, allowing only the backend modules. The
  workspace-wide test and lint sweep follows in m2.
- **Third-party std clocks.** A third-party API that takes
  `std::time::Instant` (str0m, srt-tokio, winit, qlog) converts with
  `Instant::into_std` and `Instant::from_std`, native only, through the runtime
  clock's origin: exact on a running clock, durations only on paused tokio or
  the simulator. Device threads outside any runtime keep `std::thread::sleep`
  under a per-site `#[allow]`, since they have no ambient clock to wait on
  (decided 2026-10-20 by the maintainer, in
  [#6135](https://github.com/moq-dev/moq/pull/6135)).
- **Consolidation.** perf/3122 (one clock read per io_uring turn) landed with
  moq-uring on moq-clock in [#5926](https://github.com/moq-dev/moq/pull/5926). The relay seam
  [bench-relay](/quest/m2/bench-relay.md) needed is `moq_relay::Accepted`:
  a `moq_relay::Connection` over any moq-net transport, such as the in-memory
  one. Its tests run on paused tokio until
  [the runtime trait](/quest/m2/runtime-trait.md) can spawn on the sim. Quests that already asked
  for "mocked time" Require the core quest. moq-quic takes the logical
  instant at its boundary.

This milestone verifies the exported clock contract and production consumers
needed by the in-tree QUIC stack and clustering regressions. Unrelated test/private
cleanup and the global lint ratchet belong to
[the follow-up sweep](/quest/m2/time-test-sweep.md).

Remaining: every child has landed, with public wall times on `moq_clock::Wall`
in [#6298](https://github.com/moq-dev/moq/pull/6298). Move the decisions the
m2 quests still cite into them, then retire this epic.

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - the per-flake fixes this line makes systemic
- [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) - Rust and JS clocks must agree across either browser backend
