# [L] moq-net on moq-clock

## Goal

moq-net, moq-mux, moq-stats, moq-room, and moq-ffi read time and
arm timers only through `moq-clock`:

- `moq_net::time::Instant` is `moq_clock::Instant`, and moq-net's private
  `Clock`/`Deadline` are gone in favor of moq-clock's deadline queue.
- `model/clock.rs` reads wall time through moq-clock, so `Timestamp::now`
  follows a test's `Manual` or sim clock. Its anchor (today a process-wide
  `LazyLock` of instant and wall offset) moves into the ambient clock, so each
  clock gets its own and the first test cannot fix it for later ones; a
  `step_wall()` within one clock keeps it. Test sequential clocks that start
  at different wall times.
- `rs/moq-net/sim` is deleted; moq-net's tests run on `#[moq_clock::test]`.
- `moq_mux::Clock` stays a media mapping (monotonic instant to PTS, plus the
  wall time of PTS zero) and consumes moq-clock; `arrival()` and `placed()`
  read the ambient clock instead of `SystemTime::now()`.
- These crates' `web_async::time` uses move to moq-clock; web-async remains
  only for spawn.

## Plan

Breaking; it lands on `main` after the crate (see [the line's README](/quest/m1/time/README.md)).
The ambient functions sit behind moq-net's `async` feature; the core takes
explicit instants. The migration preserves that boundary rather than exposing
ambient time to the sans-IO core.
Coordinate clock origin, units, and controlled-time behavior with the
[browser backend](/quest/m2/rs2ts/README.md) if it has landed. Source-level
`Instant` mappings depend on the selected backend; WASM retains Rust time types.

What moq-clock landed with, mapped onto moq-net: `time::Clock` becomes
`moq_clock::Deadlines` (built with its first `now`, so no `try_now`), and
`Clock::sim()` becomes `sim::attach(&deadlines)`, which keeps the queue in
step until it drops. `time::run` and `run_sim` both become `moq_clock::drive`.
`moq_net_sim::{now, sleep, sleep_until, timeout}` become the ambient
`moq_clock` functions; `spawn`, `advance`, and `yield_now` move to
`moq_clock::sim`. The sim's and `Manual`'s wall clock start at the fixed
`Manual::WALL_START` (2025-01-01 UTC), so moq-mux sees a set host clock
without any setup. `moq_clock::Wall` is wall time; `moq_net::Timestamp`
stays media time.

moq-mux's paused-clock test (`paused_clock_preserves_native_boundaries`) moves
to the sim or `Manual`, and gains a `step_wall` case: a wall step changes the
ambient wall clock, but an existing `Clock`'s PTS-zero wall mapping and its
PTS stay put, as `wall_mapping_survives_a_system_clock_adjustment` requires
(archived records keep their wall times); only a newly created mapping
samples the corrected wall time. The anchor's random jitter stays as is;
[seeded sim](/quest/m2/sim-seed.md) owns randomness.

Ban direct clock reads and timers in these crates (the ratchet). Update
`rs/moq-net/AGENTS.md`'s test rule and every doc that names `moq_net_sim`.
Report the API break in the PR.

Public API: breaking. Every public signature in these crates that takes or
returns `std::time::Instant` moves to `moq_clock::Instant`, since a logical
instant has no general conversion back: `time::Instant`, `moq_mux::Clock`,
and moq-mux's `flush`/`update` family among them (`container::Producer`,
`catalog::Estimator`, `rate::Control`, the codec and import producers). The
PR enumerates each break and migrates its in-tree callers. Wire: none.
