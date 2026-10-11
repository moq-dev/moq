# Controlled time

## Goal

Every MoQ crate and package reads time and arms timers only through
`moq-time` (Rust) or `@moq/time` (JS), and every behavioral test runs on a
controlled clock: no wall-clock reads and no real sleeps. The backend is
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
- **A new published crate, `moq-time`,** extracted from moq-net's private
  `Clock`/`Deadline`, not a public `moq_net::time` (mux, downstream apps, and other
  crates would depend on moq-net for time) and not web-async (a separate repo,
  and the thing the runtime trait replaces).
- **The instant is logical:** `Instant(u64)` nanoseconds from the clock's
  origin, so a test starts at zero with fixed coordinates and never reads the
  host clock. Native, wasm, and io_uring instants convert at the backend.
  Nanoseconds match `Duration` and QUIC pacing. In JS a plain `number` of
  nanoseconds is exact for 104 days of uptime and only loses sub-microsecond
  precision after; rs2ts maps `moq_time::Instant` to `@moq/time`'s `Instant`,
  not to `U64`. Its methods mirror `std::time::Instant`'s, so swapping one for
  the other is an import change.
- **Wall time is a separate type from the same clock.** The manual clock's
  `advance()` moves both; `step_wall()` moves only wall time, so a test can
  prove a clock correction never manufactures elapsed (billable) time.
- **Runtime-ambient access.** State machines keep taking `now` (moq-net's
  `Driver::poll(now, waiter)` and the sans-IO line's `tick(now)`). Async glue
  calls `moq_time::now()`, `sleep()`, `timeout()`, and `interval()`, which
  resolve to the current runtime's clock: one clock per runtime, nothing to
  plumb, no process-global test override. The ambient functions stay behind
  moq-net's `async` feature, so the sans-IO core keeps explicit instants.
- **The simulator folds into `moq-time`** behind a `sim` feature (a
  dev-dependency only, so production builds never compile it). It is one
  backend of the ambient clock, starts at zero, keeps deadlock detection, and
  seeds the later runtime trait. `#[moq_time::test]` replaces
  `#[moq_net_sim::test]`.
- **Tests move onto the sim wherever possible.** A test with no real I/O
  that cannot run on the sim yet uses paused tokio, whose clock `moq-time`
  reads. A real-I/O test never uses paused tokio, which auto-advances to the
  next timer while the OS event is still in flight; it syncs on events, with
  wall time only for its watchdog.
- **Landing.** Everything lands on `main`, the trunk. The crate and the JS
  package land first and additively; the migrations that change a published
  type (moq-net's `time::Instant`, moq-mux's `Clock`, `@moq/net`'s time
  units) follow as breaking changes. No backport: `release` is touched only
  to cut a release or backport a critical fix, so pinned consumers adopt
  `moq-time` from the next cut (decided 2026-10-10 from review, replacing an
  earlier backport plan).
- **Lint ratchet.** Each migration quest bans direct clock reads and timers in
  the crates and packages it finishes: clippy `disallowed-methods` and
  `disallowed-types` in a per-crate `clippy.toml`, and a biome
  `noRestrictedGlobals` override, allowing only the backend modules. This
  README flips the ban workspace-wide at the end.
- **Consolidation.** perf/3122 (one clock read per io_uring turn) merged into
  [moq-uring on moq-time](/quest/m1/time/uring.md). The relay seam
  [bench-relay](/quest/m1/bench-relay.md) needed is
  [the relay seam](/quest/m1/time/relay-seam.md). Quests that already asked
  for "mocked time" Require the core quest. moq-quic adopts the logical
  instant in the [switch](/quest/m1/quic/fork/switch.md), which Requires the
  crate.

This README's own work, after the children: flip the lint ban workspace-wide;
state the real-I/O rule and the `moq-time` testing rule in `AGENTS.md`,
`rs/AGENTS.md`, `rs/moq-net/AGENTS.md`, and `js/AGENTS.md`; and run
`just check --all` several times on a loaded machine.

## Required

- [The moq-time crate](/quest/m1/time/crate.md) - a logical clock, deadlines, ambient tokio and browser backends, and the simulator, published on `main`
- [@moq/time](/quest/m1/time/js.md) - the same model in JS, with an installable clock
- [moq-net on moq-time](/quest/m1/time/net.md) - moq-net, moq-mux, moq-stats, moq-room, and moq-ffi run on moq-time and moq-net-sim is gone
- [JS on @moq/time](/quest/m1/time/js-migrate.md) - every JS package reads time only through `@moq/time`
- [Every crate on moq-time](/quest/m1/time/crates.md) - every remaining crate reads time only through moq-time, and its tests run on the sim, paused tokio, or events
- [moq-uring on moq-time](/quest/m1/time/uring.md) - one clock read per worker turn and moq-time's deadline queue replace the private timer heap
- [The relay seam](/quest/m1/time/relay-seam.md) - one in-memory session and a generic `moq_relay::Connection`, so relay tests run on the sim

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - the per-flake fixes this line makes systemic
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - its hand-written timer glue is `@moq/time`
- [Sans-IO moq-net](/quest/m1/rs2ts/sans-io/README.md) - the core keeps explicit instants; ambient time stays behind `async`
