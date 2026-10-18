---
title: moq-clock
description: One clock for time, timers, and deterministic tests
---

# moq-clock

[![crates.io](https://img.shields.io/crates/v/moq-clock)](https://crates.io/crates/moq-clock)
[![docs.rs](https://docs.rs/moq-clock/badge.svg)](https://docs.rs/moq-clock)

One clock for reading time and arming timers, so the same code runs on tokio,
in the browser, and in a simulator that tests drive.

## The clock

`moq_clock::Instant` is a monotonic instant, counted in nanoseconds from the
clock's origin, with the methods of `std::time::Instant`. `Wall` is wall
time from the same clock. It is a separate type because wall time can step
forward or back without any time passing, so it never measures elapsed time.

Async code reads the ambient clock: `now()`, `wall()`, `sleep()`,
`sleep_until()`, `timeout()`, `interval()`, and `drive()` resolve to the
current runtime's clock. That is tokio's on native, including paused tokio,
and `performance.now()` with `setTimeout` in the browser. Nothing is plumbed
through. On native, every unpaused runtime and every plain thread share one
clock, so their instants compare. A paused tokio runtime has its own clock,
and it pauses the instant but not wall time. The simulator is its own clock
too.

Instants from different clocks don't compare, and there is no conversion back
to `std::time::Instant`.

## Caller-driven code

A state machine polled as `poll(now, waiter)` never reads a clock. It owns a
`Deadlines` queue and arms `Deadline`s on it. Each poll advances the queue to
`now`, polls its work, then parks, which returns the earliest deadline as the
instant to be polled by. `drive()` runs such a machine on the ambient clock.

To test one directly, step a `Manual` clock and pass `manual.now()` in.
`advance()` moves both instant and wall time. `step_wall()` moves only wall
time, so a test can show that a clock correction never creates elapsed time.

## Testing on the simulator

The `sim` feature adds a single-threaded, deterministic executor. Enable it
for dev-dependencies only:

```toml
[dev-dependencies]
moq-clock = { version = "0.11", features = ["sim"] }
```

```rust
use std::time::Duration;

#[moq_clock::test]
async fn gives_up_after_five_seconds() {
    let task = moq_clock::sim::spawn(async {
        moq_clock::timeout(Duration::from_secs(5), std::future::pending::<()>()).await
    });
    assert!(task.await.unwrap().is_err());
    assert_eq!(moq_clock::now(), moq_clock::Instant::ZERO + Duration::from_secs(5));
}
```

Inside the simulator the clock starts at `Instant::ZERO` and 2025-01-01 UTC
(`Manual::WALL_START`), like a new `Manual`. Time moves only when every task
has stalled: the clock jumps to the earliest armed timer, so a test of a 30
second timeout takes no real time. A run where every task is parked and no
timer is armed panics as a deadlock instead of hanging. `sim::advance()` and
`sim::step_wall()` move time by hand, and `sim::attach(&deadlines)` keeps a
`Deadlines` queue in step until it drops.

## Driving a tokio runtime by hand

Paused tokio and the simulator both jump to the next timer once everything
idles. A harness outside the runtime, like a Python test loop driving the
bindings, needs time that moves only when it says so. The `manual` feature adds
`Manual::install(&handle)`, which makes a `Manual` clock the ambient clock of
one tokio runtime. Its timers fire only when the returned handle's `advance()`
reaches them, and other runtimes and threads keep the host clock. Python's
`moq.testing.Clock` wraps it in builds made with moq-ffi's `testing` feature.

Tests that do real I/O (sockets, browsers, codecs) can't use the simulator.
They wait on events and use the host clock only for a watchdog.

API: [docs.rs/moq-clock](https://docs.rs/moq-clock).
