# @moq/signals

Reactive signals with explicit tracking.
No magic or footguns.

## Usage
```sh
bun add @moq/signals
```

### Signal
A `Signal` holds a reactive value.

```ts
import { Signal } from "@moq/signals";

const count = new Signal(0);

count.peek();      // 0
count.set(1);     // notifies subscribers
count.update(n => n + 1); // update via function

const dispose = count.subscribe(n => console.log(n)); // subscribe to changes manually
```

Updates are batched, coalescing multiple updates into a single microtask.
The old and new values are then compared with deep equality ([dequal](https://github.com/lukeed/dequal)) to avoid unnecessary wakeups.
It's possible to skip this check, but please benchmark it first...

### Effect
An `Effect` is a reactive scope.
It re-runs whenever a tracked signal changes.

```ts
import { Signal, Effect } from "@moq/signals";

const name = new Signal("world");

const effect = new Effect((effect) => {
  const value = effect.get(name); // read AND track
  console.log(`Hello, ${value}!`);
});

name.set("signals"); // effect reruns: "Hello, signals!"

effect.close(); // cleanup
```

The key difference from other libraries: **`effect.get(signal)` is what subscribes**.
If you just want to read without tracking, use `signal.peek()` directly.

### effect.cleanup
Run a cleanup function when the effect reruns or closes.

```ts
const name = new Signal("world");

const effect = new Effect((effect) => {
  const value = effect.get(name);
  console.log(`Hello, ${value}!`);

  effect.cleanup(() => console.log(`Goodbye, ${value}!`));
});
```

### effect.run
Create a nested effect that can be rerun independently.
It will be closed when the parent effect reruns or closes.

```ts
const name = new Signal("world");
const age = new Signal(20);

const effect = new Effect((effect) => {
  const n = effect.get(name);
  console.log(`Hello, ${n}!`);

  // NOTE: use the nested effect's argument, not the parent's
  effect.run((nested) => {
    const a = nested.get(age);
    console.log(`You are ${a} years old!`);
  });
});

age.set(21); // only the nested effect reruns: "You are 21 years old!"
```

### effect.spawn
Run async work that blocks the effect from re-running until it completes.
The promise should monitor the `effect.cancel` Promise to detect when the effect wants to rerun.

```ts
const url = new Signal("/api/data");

const effect = new Effect((effect) => {
  const endpoint = effect.get(url);

  effect.spawn(async () => {
    const request = fetch(endpoint);
    const res = await Promise.race([request, effect.cancel]);
    if (!res) return; // effect cancelled, stop the request

    // ...
  });
});
```

### Helpers

Effects also provide lifecycle helpers that auto-cleanup:

- **`effect.set(signal, value, cleanup)`** - temporarily set the value of a signal for the duration of the effect
- **`effect.timer(fn, ms)`** - `setTimeout` that cancels on cleanup
- **`effect.interval(fn, ms)`** - `setInterval` that cancels on cleanup
- **`effect.animate(fn)`** - `requestAnimationFrame` that cancels on cleanup
- **`effect.event(target, type, fn)`** - `addEventListener` that removes on cleanup
- **`effect.subscribe(signal, fn)`** - shorthand: run `fn` each time `signal` changes
- **`effect.getAll(signals)`** - get the values of multiple signals, only if they are all truthy

## Framework Integrations

### Solid.js

```ts
import { createAccessor } from "@moq/signals/solid";

const count = new Signal(0);
const value = createAccessor(count); // returns a Solid Accessor
```

### React

```ts
import { useValue, useSignal } from "@moq/signals/react";

function Component() {
  const value = useValue(count); // read-only
  const [value2, setValue2] = useSignal(count); // read-write, like useState
}
```
