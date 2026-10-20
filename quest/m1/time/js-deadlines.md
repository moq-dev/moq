# [S] JS timers fire at their own deadlines

## Goal

Under a `Manual` clock, every `@moq/clock` timer observes its own deadline:

- `Manual.advanceAsync(duration)` fires each due timer with the clock at that
  timer's deadline, in deadline order, draining microtasks between them. A
  timer that re-arms itself (an interval, a retry scheduling its next attempt)
  counts from the moment it fired, so one `advanceAsync` replaces a loop of
  small steps.
- `Effect.interval(fn, period)` arms its first deadline before it returns, so a
  synchronous `advance(period)` right after it runs `fn` at `period`, not at
  `2 * period`.

Each fix lands with a regression test that fails without it, on the manual
clock with no sleeps or wall time.

Out of scope: Rust's `moq_clock::sim::advance` and `Installed::advance`, which
also jump. A Rust test already gets stepped time from the sim's auto-advance by
sleeping in the test task; JS has no idle detection, so `advanceAsync` is the
only driver and is where stepping matters.

## Plan

Reproduced on `main` (`9a2663e72`) with a `Manual` clock:

- A task that does `await sleep(10)` three times, under one
  `advanceAsync(30)`, records `[30]` instead of `[10, 20, 30]`: the first sleep
  fires at 30 and the second re-arms for 40.
- `effect.interval(fn, 10)`, then `advance(10)`, `advanceAsync(0)`, and
  `advanceAsync(30)` runs `fn` at `[40]` instead of `[10, 20, 30, 40]`. The
  Interval's immediate first tick resolves a microtask after arming, so the
  synchronous advance makes it look late and `Delay` pushes the next deadline
  to 20; the jump then fires that at 40. The two bugs compound.

Decided 2026-10-20, with reasons:

- **`advanceAsync` steps; `advance` still jumps.** `advanceAsync` already
  models a live event loop (it drains microtasks), so it walks the timer heap
  to each deadline up to the target, the way tokio's paused clock
  auto-advances when idle. Synchronous `advance` cannot drain between
  deadlines, so it stays a single jump and remains the way to model a stalled
  loop (the existing missed-tick interval tests already use it). No new method:
  a second "stepped" variant would leave the default wrong.
- **The first interval period is armed synchronously,** rather than documenting
  a microtask-late first tick that every caller would have to work around.
  `Effect.interval` starts its `Interval` at `now + period` instead of
  consuming an immediate tick (tokio's `interval_at`). Keep that start private
  unless a second consumer needs it; exporting `intervalAt` would also want a
  Rust mirror.
- **`fire()` is unchanged.** It already advances exactly to the earliest
  deadline; check it still composes with the stepping loop.
- **Breaking behavior for `@moq/clock`:** a test that relied on
  `advanceAsync` firing every due timer at the target instant switches to
  `advance()`. Add a `MIGRATE.md` entry and update the `advance`/`advanceAsync`
  paragraph in `doc/lib/js/clock.md`. Audit in-tree `advanceAsync` callers
  (`js/net`, `js/hang`, `js/signals`) for assertions that encoded the jump.

Watch for: a timer armed while draining joins the walk when it is due by the
target, and the clock never ends past the target or moves backward.

## Related

- [Every crate on moq-clock](/quest/m1/time/crates.md) - the Rust side of the same clock contract
- [Time test sweep](/quest/m2/time-test-sweep.md) - legacy JS tests converted there get stepped `advanceAsync` for free
