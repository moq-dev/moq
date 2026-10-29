---
title: '@moq/clock'
description: Logical time and timers with a manual clock for deterministic tests
---

# @moq/clock

[![npm](https://img.shields.io/npm/v/@moq/clock)](https://www.npmjs.com/package/@moq/clock)
[![API](https://img.shields.io/badge/API-reference-blue)](/lib/js/api/clock)

Use `@moq/clock` when a deadline and the elapsed time beside it must come from
one clock. Its default backend reads the host's monotonic and wall clocks;
a test installs a `Manual` clock without patching the host's timers.

```sh
bun add @moq/clock
```

```ts
import { Manual, Milli, Nano, now, sleep } from "@moq/clock";

const clock = new Manual();
const installed = clock.install();
try {
    const start = now();
    const pending = sleep(Nano.fromMilli(Milli(10)));
    await clock.advance(Nano.fromMilli(Milli(10)));
    await pending;
    console.log(now() - start); // 10,000,000 nanoseconds
} finally {
    installed.close();
}
```

`Instant` counts nanoseconds from its clock's origin: the realm's performance
time origin on the host, so DOM timestamps convert by unit, or zero for a
`Manual` clock. `Nano`, `Micro`,
`Milli`, and `Second` brand duration units; convert a duration to nanoseconds
before passing it to a timer. Number nanoseconds are exact for roughly 104
days, then gradually lose sub-microsecond precision. Compare instants only
from the same clock. Rust and JS each own their origin: no instant may cross
that boundary until the backend explicitly shares one.

`Wall` is a separate branded number of milliseconds since the Unix epoch.
Use it for dates, rather than measuring elapsed time. A Rust wall-time
boundary must convert its units explicitly. A new `Manual` starts at instant
zero and 2025-01-01 UTC. `await advance()` moves both readings through each due
timer deadline, draining tasks between deadlines, including timers those tasks
re-arm before the target. `yieldNow()` waits for a later host task, giving the
host's other work a turn without ordering it; a `Manual` clock moves time only
once every pending yield has resumed, so a loop that yields mid-work never sees
a later time. `jump()` moves straight to the target and fires due
timers late without draining, modeling a stalled event loop. `stepWall()` changes
only wall time. `fire()` waits for a timer
to be armed, even by a later host task such as I/O, then advances exactly to the
earliest deadline the same way; it never resolves if nothing arms one.
`drive(work)` fires deadlines one at a time until `work` settles, then settles
with it, for work whose timers a test cannot count; time stops at the deadline
it settled on, so a timer armed afterwards waits for the next explicit call.
Drive each clock serially: until `advance()`, `fire()`, or `drive()` settles,
another one rejects and `jump()` throws without moving time.
Timer callbacks may arm timers, but must leave driving that clock to their caller.

`origin()` reads the wall time at instant zero, which maps an instant onto
wall time without the jitter of subtracting two live reads. On the host it is
the realm's performance time origin; on a `Manual` it follows `stepWall()`.

Installation is scoped to the current JS realm. Close the handle `install()`
returns in `finally`, and keep tests sharing that realm serial. Timers remain bound to the clock on
which they were created, even after restoration. Host timers used by a test
runner or RPC framework keep running in real time.

An interval's first `tick()` is ready at once. Its default `Burst` policy
returns every missed scheduled tick; `Delay` resumes a full period from now,
and `Skip` resumes at the next original deadline. Keep one pending tick per
handle and call `close()` to cancel it when finished. Sleep accepts an abort
`signal`, and `ref: false` lets a Node or Bun process exit while only that
sleep is pending. Timeout cancels its own timer when the operation settles, but does
not cancel the operation. An already-ready operation wins a zero timeout.

Every `@moq/*` package reads time and arms timers through this package, so
installing a `Manual` clock also drives their timeouts and retention. See the [source API](https://github.com/moq-dev/moq/tree/main/js/clock/src)
for the exported functions and conversions.
