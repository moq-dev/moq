---
title: "@moq/signals"
description: The reactive primitives under every @moq package
---

# @moq/signals

[![npm](https://img.shields.io/npm/v/@moq/signals)](https://www.npmjs.com/package/@moq/signals)

Reactive signals with explicit tracking and automatic cleanup. Every `@moq`
package exposes its state through these, so this is how you drive and observe
the rest.

| Class | What it is |
| --- | --- |
| `Signal<T>` | A mutable observable value: `peek()`, `set()`, `update()`, `subscribe()`, `changed()`. |
| `Computed<T>` | A read-only value derived from other signals. |
| `Effect` | A scope that reruns when a signal it read changes, and tears down what it set up. |
| `Once<T>` | Terminal state that settles once, observable and awaitable. Used for `closed`. |
| `Derived` | A cheap mapped view with no lifecycle. |

```ts
import { Effect, Signal } from "@moq/signals";

const volume = new Signal(1);

const effect = new Effect((effect) => {
    const v = effect.get(volume);            // read AND subscribe; peek() reads without subscribing
    effect.interval(() => console.log(v), 1000);  // cancelled on rerun or close
    effect.cleanup(() => console.log("bye"));
});

volume.set(0.5);   // reruns
effect.close();    // permanent
```

The rules that differ from other signal libraries:

- **Nothing is tracked implicitly.** `effect.get(signal)` subscribes; `signal.peek()` doesn't.
- **Writes coalesce per microtask** and only notify on a real change (deep for plain objects, identity for class instances).
- **Effects own their resources.** `effect.timer`, `interval`, `animate`, `event`, `spawn`, and `run` (a nested effect) all clean up on rerun or close, so never call `setTimeout` or `addEventListener` inside one directly. A rerun waits for the previous run's `spawn` tasks to settle, and `effect.abort`/`effect.cancel` tell them to stop.
- **Dev builds warn** about effects that tracked nothing, effects garbage-collected without `close()`, and signals leaking subscribers.

Components follow one shape: `in` (wired inputs), `out` (read-only derived
state), and public writable knobs. `getter()` and `Inputs<T>` accept a raw
value, a signal, or another component's output interchangeably.

Adapters: `@moq/signals/react` (`useValue`, `useSignal`), `@moq/signals/solid`
(`createAccessor`, `createPair`), and `@moq/signals/dom` for building reactive
DOM without a framework, which is what the UI overlays use.

```tsx
import { useValue } from "@moq/signals/react";

function Volume({ watch }) {
    const volume = useValue(watch.controls.volume);
    return <input type="range" value={volume} onChange={(e) => watch.controls.volume.set(+e.target.value)} />;
}
```
