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

### Computed

A `Computed` is a read-only signal derived from other signals.
The compute function receives an `Effect` and reads its dependencies with `effect.get(...)`, exactly like a normal effect.

```ts
import { Signal, Computed } from "@moq/signals";

const first = new Signal("Ada");
const last = new Signal("Lovelace");

const full = new Computed((effect) => `${effect.get(first)} ${effect.get(last)}`);

full.peek(); // "Ada Lovelace"
first.set("Grace");
// reads inside an effect are always fresh; a bare peek() is fresh on the next microtask
```

The value is computed lazily on first read, so it is never `undefined`.
It recomputes when a dependency changes, caching the result, and downstream notifications are coalesced and equality-filtered like any signal.

Keep the compute function **pure**: it should derive a value, not perform side effects. Use an `Effect` for side effects.

A computed that reads its own value (directly or through a cycle like `a` → `b` → `a`) throws instead of looping forever.

A standalone `Computed` must be closed to stop tracking its dependencies:

```ts
const sum = new Computed((effect) => effect.get(a) + effect.get(b));
// ...
sum.close();
```

More commonly, create one inside an effect with `effect.computed(...)`, which closes it automatically when the effect reruns or closes:

```ts
const effect = new Effect((effect) => {
  const total = effect.computed((e) => e.get(a) + e.get(b));
  console.log(effect.get(total));
});
```

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

### effect.abort

An `AbortSignal` that is aborted when the effect reruns or closes.
Pass it to any API that accepts an `AbortSignal` — `fetch`, `addEventListener`, streams, etc.

```ts
const url = new Signal("/api/data");

const effect = new Effect((effect) => {
  const endpoint = effect.get(url);

  effect.spawn(async () => {
    const res = await fetch(endpoint, { signal: effect.abort });
    // automatically aborted on rerun/close

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
- **`effect.event(target, type, fn)`** - `addEventListener` that removes on cleanup/rerun via `AbortSignal`
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
