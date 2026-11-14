# @moq/clock

Logical monotonic time, wall time, and timers from one clock per JavaScript realm.
Use `Manual` from `@moq/clock/test` to test time without replacing the host's timers.

```ts
import { Duration, Instant, now, sleep } from "@moq/clock";
import { Manual } from "@moq/clock/test";

const clock = new Manual();
const installed = clock.install();
try {
    const pending = sleep(Duration.millis(10));
    await clock.advance(Duration.millis(10));
    await pending;
    console.log(now().since(Instant.ZERO).asMillis()); // 10
} finally {
    installed.close();
}
```

See the [guide](https://moq.dev/lib/js/clock) for origins, interval behavior,
and the distinction between elapsed time and wall time.
