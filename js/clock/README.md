# @moq/clock

Logical monotonic time, wall time, and timers from one clock per JavaScript realm.
Use `Manual` to test time without replacing the host's timers.

```ts
import { Manual, Milli, Nano, now, sleep } from "@moq/clock";

const clock = new Manual();
const installed = clock.install();
try {
    const pending = sleep(Nano.fromMilli(Milli(10)));
    await clock.advance(Nano.fromMilli(Milli(10)));
    await pending;
    console.log(now()); // 10,000,000 nanoseconds
} finally {
    installed.close();
}
```

See the [guide](https://moq.dev/lib/js/clock) for origins, interval behavior,
and the distinction between elapsed time and wall time.
