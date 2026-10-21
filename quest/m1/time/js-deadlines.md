# [S] JS timers fire at their own deadlines

## Goal

Under a `Manual` clock, every `@moq/clock` timer observes its own deadline:

- `await Manual.advance(duration)` fires each due timer with the clock at that
  timer's deadline, in deadline order, draining microtasks between them. A
  timer that re-arms itself (an interval, a retry scheduling its next attempt)
  counts from the moment it fired, so one `advance` replaces a loop of small
  steps.
- `Manual.jump(duration)` keeps today's synchronous single jump, the way to
  model a stalled event loop.
- `Effect.interval(fn, period)` arms its first deadline before it returns, so a
  `jump(period)` right after it runs `fn` at `period`, not at `2 * period`.

Each fix lands with a regression test that fails without it, on the manual
clock with no sleeps or wall time.

Out of scope: Rust's `moq_clock::sim::advance` and `Installed::advance`, which
also jump. A Rust test already gets stepped time from the sim's auto-advance by
sleeping in the test task; JS has no idle detection, so `advance` is the
only driver and is where stepping matters.

## Plan

Reproduced on `main` (`9a2663e72`) with a `Manual` clock:

- A task that does `await sleep(10)` three times, under one
  `advanceAsync(30)`, records `[30]` instead of `[10, 20, 30]`: the first sleep
  fires at 30 and the second re-arms for 40.
- `effect.interval(fn, 10)`, then sync `advance(10)`, `advanceAsync(0)`, and
  `advanceAsync(30)` runs `fn` at `[40]` instead of `[10, 20, 30, 40]`. The
  Interval's immediate first tick resolves a microtask after arming, so the
  synchronous advance makes it look late and `Delay` pushes the next deadline
  to 20; the jump then fires that at 40. The two bugs compound.

Decided 2026-10-20, with reasons:

- **Rename: `advanceAsync` becomes `advance`, and sync `advance` becomes
  `jump`** (maintainer decision on the planning PR). The async method is the
  one tests should reach for, so it takes the short name, matching tokio's
  `time::advance`. It walks the timer heap to each deadline up to the target,
  like tokio's paused clock when it auto-advances, because it already drains
  microtasks and so models a live event loop. `jump` cannot drain between
  deadlines, so it moves straight to the target and fires whatever is due
  there late; the existing missed-tick interval tests use it for that. No
  stepped variant beside a jumping default: that would leave the default wrong.
- **The first interval period is armed synchronously,** rather than documenting
  a microtask-late first tick that every caller would have to work around.
  `Effect.interval` lives in `@moq/signals` and cannot reach `Interval`'s
  private deadline, so it drops `Interval` and loops on the exported
  `sleepUntil(next)`, which arms synchronously. It starts at `now + period`
  and computes the `Delay` deadline itself: `max(scheduled + period, now +
  period)`. This needs no `@moq/clock` API change; exporting an `intervalAt`
  or changing `Interval`'s first tick would add public surface and want a Rust
  mirror. Unlike an `Interval`, which keeps the clock it was created on, each
  `sleepUntil` reads the clock installed at that moment; document that on
  `Effect.interval`. Keep `Interval`'s synchronous `RangeError` on a zero
  period, with a regression test: a zero-period loop re-arms an already due
  sleep forever, starving the event loop and never letting `advance` settle.
- **`fire()` keeps its behavior,** advancing exactly to the earliest deadline;
  its doc comment and body call `advanceAsync` and move to `advance`. Check it
  still composes with the stepping loop.
- **Breaking API for `@moq/clock`.** Every call site renames: about 60
  `advanceAsync` and 42 sync `advance` calls under `js/`, tests in every
  package included. Audit them for assertions that encoded the jump while
  renaming. An entry under `## Unreleased` in `js/clock/MIGRATE.md` covers the
  rename and the stepping, and `doc/lib/js/clock.md` (the example and the
  `advance`/`advanceAsync`/`fire` paragraph) and `js/clock/README.md` follow.
- **Rust names stay.** `moq_clock::Manual::advance` arms no timers, so it has
  no step-or-jump split to name. `moq_clock::sim::advance` is already the async
  `advance`. `Installed::advance` is the sync jump and would become `jump` only
  alongside making Rust step, which is out of scope.

Watch for: a timer armed while draining joins the walk when it is due by the
target, and the clock never ends past the target or moves backward.

## Related

- [Every crate on moq-clock](/quest/m1/time/crates.md) - the Rust side of the same clock contract
- [Time test sweep](/quest/m2/time-test-sweep.md) - legacy JS tests converted there get stepped `advance` for free
