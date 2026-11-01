# [S] Timing-bound tests pass beside a busy build

## Goal

Four tests that pass alone but have failed under a loaded `just check` pass
reliably there, each fixed at its cause under the epic's rules:
moq-c's `video_raw_*` tests, moq-cli's `epochs::*` route-change bounds,
kio's `log::test` eviction tests, and
`sh/dispatch-query.test.py`'s
`test_terminating_the_run_stops_the_running_phase`.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985), [moq-dev/moq#7036](https://github.com/moq-dev/moq/pull/7036), and [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

They share no fixture, but each fails only under load, so they land
together: three are wall-clock bounds a starved host misses, and kio's
eviction tests depend on global state other tests in the same binary hold.
Reproduce each under load first (for example
beside a `cargo build` of the workspace, or with `stress-ng`), and fix the
cause rather than raising a bound.

- **moq-c `video_raw_*`** (`rs/moq-c/src/test.rs:4913` onward): timed out
  while `kio-macros::ui` compiled the whole workspace-hack beside them in
  #6985's land runs. Callbacks wait `TIMEOUT` (10 s, `test.rs:8`) on wall
  time, and some assert silence with a 50 ms `recv_timeout`. Find what the
  starved path waits on (decode thread, encoder, or the runtime) before
  changing the wait.
- **moq-cli `epochs::*`** (`rs/moq-cli/tests/integration/epochs.rs:16`):
  `routing()` is twice `origin::Config::default().update_hold` plus 1 ms,
  601 ms of wall time across a real relay and real sockets. Drive the holds
  on a manual or simulated clock, or assert the route change by event, so
  the bound measures the hold rather than the scheduler.
- **kio `log::test`** (`rs/kio/src/log.rs:806`, `:824`, `:980`):
  `reader_outlives_writer`, `a_read_outlasts_eviction`, and
  `eviction_frees_whatever_readers_do` failed in the parallel run of #7036.
  They take `COLLECTOR` and then `collect()` the global `crossbeam_epoch`
  collector, which other tests in the same binary may still pin. Give these
  tests a local `crossbeam_epoch::Collector`, or otherwise make the reclaim
  they assert independent of other threads.
- **`sh/dispatch-query.test.py:347`**: failed under host load about 27 in
  #7360. With `CHECK_JOBS=3` it reads stderr non-blocking right after the
  stub opens the FIFO and asserts `ci: check rs started` is already there,
  which races the writer. Wait for the line instead of sampling it once.

Public API: none. Wire: none.

## Related

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - its `kio-macros` edge was the load that exposed the first two
