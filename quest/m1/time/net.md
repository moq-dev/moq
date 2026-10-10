# [L] moq-net on moq-time

## Goal

moq-net, moq-mux, moq-stats, moq-room, and moq-ffi read time and
arm timers only through `moq-time`:

- `moq_net::time::Instant` is `moq_time::Instant`, and moq-net's private
  `Clock`/`Deadline` are gone in favor of moq-time's deadline queue.
- `model/clock.rs` reads wall time through moq-time, so `Timestamp::now`
  follows a test's `Manual` or sim clock. Its anchor (today a process-wide
  `LazyLock` of instant and wall offset) moves into the ambient clock, so each
  clock gets its own and the first test cannot fix it for later ones; a
  `step_wall()` within one clock keeps it. Test sequential clocks that start
  at different wall times.
- `rs/moq-net/sim` is deleted; moq-net's tests run on `#[moq_time::test]`.
- `moq_mux::Clock` stays a media mapping (monotonic instant to PTS, plus the
  wall time of PTS zero) and consumes moq-time; `arrival()` and `placed()`
  read the ambient clock instead of `SystemTime::now()`.
- These crates' `web_async::time` uses move to moq-time; web-async remains
  only for spawn.

## Plan

Breaking; it lands on `main` after the crate (see [the line's README](/quest/m1/time/README.md)).
The ambient functions sit behind moq-net's `async` feature; the core takes
explicit instants. Decided 2026-10-10 from review: this waits for
[the async feature](/quest/m1/rs2ts/sans-io/async-feature.md), so the
migration lands gated rather than exposing ambient time to the sans-IO core.
Tell rs2ts how `moq_time::Instant` maps to `@moq/time` if the
[translator](/quest/m1/rs2ts/translator.md) has landed.

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
returns `std::time::Instant` moves to `moq_time::Instant`, since a logical
instant has no general conversion back: `time::Instant`, `moq_mux::Clock`,
and moq-mux's `flush`/`update` family among them (`container::Producer`,
`catalog::Estimator`, `rate::Control`, the codec and import producers). The
PR enumerates each break and migrates its in-tree callers. Wire: none.

## Required

- [The moq-time crate](/quest/m1/time/crate.md) - the clock and sim this adopts
- [The async feature](/quest/m1/rs2ts/sans-io/async-feature.md) - the feature the ambient functions sit behind
