# [L] The moq-time crate

## Goal

A published `rs/moq-time` crate gives the workspace one clock:

- `Instant`, a logical `u64` of nanoseconds from the clock's origin, with
  `std::time::Instant`'s methods, and a separate wall-clock `Timestamp`.
- `Manual`, a clock tests drive with `advance(Duration)`, which moves both,
  and `step_wall(...)`, which moves only wall time, forward or back.
- The deadline queue extracted from moq-net's private `Clock`/`Deadline`, for
  caller-driven code: `poll(now, waiter)` returns the next deadline.
- Ambient `now()`, `wall()`, `sleep()`, `sleep_until()`, `timeout()`, and
  `interval()` that resolve to the current runtime's clock, with tokio
  (paused tokio included) and browser wasm backends.
- The simulator from `rs/moq-net/sim` behind a `sim` feature, starting at
  zero, with `#[moq_time::test]`.
- `doc/lib/rs/moq-time.md` explains the clock, the sim, and how to test with
  them.

It lands additively on `main`: nothing else changes type yet.

## Plan

The design decisions are in [the line's README](/quest/m1/time/README.md).
Settle in the PR:

- Timer semantics, each with a test on `Manual` and the sim: cancellation,
  rearming, an already-expired deadline fires at once, equal deadlines fire in
  arming order, and `interval` catch-up (default to tokio's `Burst` unless a
  consumer needs otherwise).
- How the tokio backend reports a logical instant: an origin captured once per
  runtime, so paused tokio stays deterministic, and the multi-thread runtime
  [FFI runtime](/quest/m1/ffi-runtime.md) moves to stays correct.
- The browser backend reuses web-async's wasmtimer plumbing rather than a new
  shim.
- The sim keeps everything moq-net relies on today: stall-driven advance,
  `attach`, `drive`, `spawn`, deadlock detection, and the per-poll hook
  [spinning loops](/quest/m1/spinning-loops.md) adds. moq-net keeps
  `moq-net-sim` until [moq-net on moq-time](/quest/m1/time/net.md), since its
  `Instant` is still std's.
- Name the ambient functions and the test macro by role, and propose
  alternatives in the PR if any feels awkward.

Ban direct clock reads in `moq-time` itself, outside its backend modules
(the first `clippy.toml` of the ratchet). Report the new public API in the PR.

Public API: a new crate. Wire: none.
