---
title: '@moq/clock'
description: Logical time and timers with a manual clock for deterministic tests
---

# @moq/clock

Use `@moq/clock` when a deadline and the elapsed time beside it must come from
one clock. Its default backend reads the host's monotonic and wall clocks;
a test installs a `Manual` clock without patching the host's timers.

```sh
bun add @moq/clock
```

```ts
import { Manual, Milli, Nano, now, sleep } from "@moq/clock";

const clock = new Manual();
const restore = clock.install();
try {
    const start = now();
    const pending = sleep(Nano.fromMilli(Milli(10)));
    await clock.advanceAsync(Nano.fromMilli(Milli(10)));
    await pending;
    console.log(now() - start); // 10,000,000 nanoseconds
} finally {
    restore();
}
```

`Instant` counts nanoseconds from its clock's first read. `Nano`, `Micro`,
`Milli`, and `Second` brand duration units; convert a duration to nanoseconds
before passing it to a timer. Number nanoseconds are exact for roughly 104
days, then gradually lose sub-microsecond precision. Compare instants only
from the same clock. Rust and JS each own their origin: no instant may cross
that boundary until the backend explicitly shares one.

`Wall` is a separate branded number of milliseconds since the Unix epoch.
Use it for dates, rather than measuring elapsed time. A Rust wall-time
boundary must convert its units explicitly. A new `Manual` starts at instant
zero and 2025-01-01 UTC. `advance()` moves both readings; `stepWall()` changes
only wall time. `advanceAsync()` also drains due timers and the microtasks
they schedule, so assertions observe their effects.

Installation is scoped to the current JS realm. Restore it in `finally` and
keep tests sharing that realm serial. Timers remain bound to the clock on
which they were created, even after restoration. Host timers used by a test
runner or RPC framework keep running in real time.

An interval's first `tick()` is ready at once. Its default `Burst` policy
returns every missed scheduled tick; `Delay` resumes a full period from now,
and `Skip` resumes at the next original deadline. Keep one pending tick per
handle and call `close()` to cancel it when finished. Sleep accepts an abort
signal; timeout cancels its own timer when the operation settles, but does
not cancel the operation. An already-ready operation wins a zero timeout.

The package lands additively. `@moq/net` currently retains its own time units.
See the [source API](https://github.com/moq-dev/moq/tree/main/js/clock/src)
for the exported functions and conversions.
