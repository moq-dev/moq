---
title: "@moq/signals"
description: Reactive signals with explicit tracking and automatic cleanup
---

# @moq/signals

[![npm](https://img.shields.io/npm/v/@moq/signals)](https://www.npmjs.com/package/@moq/signals)

Reactive signals with explicit tracking. No magic or footguns.

`@moq/signals` is the reactive core underneath every other JavaScript package we ship. [@moq/net](/lib/js/@moq/net), [@moq/hang](/lib/js/@moq/hang/), [@moq/watch](/lib/js/@moq/watch), and [@moq/publish](/lib/js/@moq/publish) all expose their state as signals, so learning this package is how you drive the rest.

## Overview

Three classes do the work:

| Class | What it is |
|---|---|
| `Signal<T>` | A mutable observable value. |
| `Computed<T>` | A read-only value derived from other signals. |
| `Effect` | A reactive scope that reruns when a tracked signal changes, and cleans up after itself. |

Alongside them: `Once<T>` for terminal state that settles exactly once, and `Derived` for a lifecycle-free mapped view.

The key difference from other signals libraries: **`effect.get(signal)` is what subscribes**. Nothing is tracked implicitly. If you want to read a value without subscribing to it, call `signal.peek()`.

## Installation

```bash
bun add @moq/signals
# or
npm add @moq/signals
```

## Signal

A `Signal` holds a reactive value. Construct it with `new`; there is no `signal()` factory.

```ts
import { Signal } from "@moq/signals";

const count = new Signal(0);

count.peek(); // 0, without subscribing
count.set(1); // notifies subscribers
count.update((n) => n + 1); // transform the previous value

const dispose = count.subscribe((n) => console.log("count is", n));
dispose(); // unsubscribe
```

### Writes are coalesced

Multiple writes in the same microtask collapse into one notification, and subscribers only fire when the value actually changed. A value that moves and moves back within a microtask notifies nobody:

```ts
const count = new Signal(0);
count.subscribe((n) => console.log(n)); // never called below

count.set(1);
count.set(0); // net change is zero, so no notification
```

Equality is deep for plain objects and arrays, but identity (`===`) for class instances. Two distinct `Broadcast` instances are never equal even if they look alike, which is what you want for handles.

Override the check when you need to:

```ts
count.set(0, true); // always notify
count.set(0, false); // never notify
```

`set` stores whatever you hand it, including a function. Use `update` to transform instead:

```ts
const fn = new Signal<() => void>(() => {});
fn.set(() => console.log("hi")); // stores the function as the value
count.update((n) => n + 1); // calls the function with the previous value
```

`mutate` edits the current value in place and returns whatever the callback returns, notifying afterwards. Reach for it when the value is a container you would rather not clone:

```ts
const peers = new Signal(new Set<string>());
const added = peers.mutate((set) => {
	const before = set.size;
	set.add("alice");
	return set.size !== before;
});
```

### Reading over time

Three ways to observe, depending on what you need:

```ts
// Every change, until you dispose.
const dispose = count.subscribe((n) => console.log("changed to", n));

// The next change only, as a promise.
const next = await count.changed();

// The current value now (on a microtask), and every change after.
const stop = count.watch((n) => console.log("is now", n));
```

`Signal.from` wraps a plain value but passes an existing signal straight through, which is how components accept `T | Signal<T>` props:

```ts
const a = Signal.from(5); // new Signal(5)
const b = Signal.from(count); // count itself
```

`Signal.race` resolves with the next value from whichever readable changes first:

```ts
const paused = new Signal(false);
const volume = new Signal(1);

const winner = await Signal.race(paused, volume); // boolean | number
```

Identity across package versions uses a `Symbol.for` brand rather than `instanceof`, so two copies of `@moq/signals` in one dependency tree still recognize each other's signals.

## Computed

A `Computed` is a read-only signal derived from others. Its function reads dependencies with `effect.get(...)`, exactly like an effect, and reruns whenever one changes.

```ts
import { Computed, Signal } from "@moq/signals";

const first = new Signal("Ada");
const last = new Signal("Lovelace");

const full = new Computed((effect) => `${effect.get(first)} ${effect.get(last)}`);

full.peek(); // undefined: the first run has not completed yet
await full.changed();
full.peek(); // "Ada Lovelace"
```

Two rules that catch people out:

- **The value is `undefined` until the first run completes**, and again after `close()`. Recomputes propagate on a microtask, coalesced and equality-filtered like any other signal. Read a computed inside an effect and handle the `undefined` case.
- **Keep the function pure.** It derives a value; it does not perform side effects. Use an `Effect` for those.

A standalone `Computed` must be closed to stop recomputing and release its dependencies:

```ts
full.close();
```

More often you create one on a long-lived effect, which closes it for you:

```ts
const signals = new Effect();
const initials = signals.computed((effect) => `${effect.get(first)[0]}${effect.get(last)[0]}`);

signals.close(); // also closes `initials`
```

## Effect

An `Effect` runs its function immediately (on a microtask) and reruns whenever a signal it read with `effect.get` changes.

```ts
import { Effect, Signal } from "@moq/signals";

const name = new Signal("world");

const effect = new Effect((effect) => {
	const value = effect.get(name); // read AND track
	console.log(`Hello, ${value}!`);
});

name.set("signals"); // reruns: "Hello, signals!"

effect.close(); // permanent: runs all cleanup and unsubscribes
```

`effect.getAll` reads several signals at once and returns `undefined` if any of them is falsy, which collapses the usual pile of guards:

```ts
const broadcast = new Signal<string | undefined>(undefined);
const track = new Signal<string | undefined>(undefined);

new Effect((effect) => {
	const values = effect.getAll([broadcast, track]);
	if (!values) return;

	const [b, t] = values; // both are strings here
	console.log(`${b}/${t}`);
});
```

It stops at the first falsy signal, so the ones after it are not tracked on that run. Above, setting `track` while `broadcast` is still `undefined` does not rerun the effect. That is short-circuit tracking, the same as an `effect.get` behind an `if`, and it costs nothing: the values are only used once every signal is truthy, and reaching that state means changing the falsy one, which reruns the effect and reads the rest. Setting `track` first and `broadcast` second still lands on both values.

### Cleanup

`effect.cleanup(fn)` registers teardown. Everything registered during a run is torn down before the next run and again on `close()`.

```ts
const url = new Signal("wss://relay.example.com");

new Effect((effect) => {
	const socket = new WebSocket(effect.get(url));
	effect.cleanup(() => socket.close());
});
```

A run that is already over runs `fn` **immediately**. That is what an async task resuming after a rerun sees, so registering teardown is enough to own a resource: register it unconditionally rather than checking staleness first.

```ts
new Effect((effect) => {
	effect.spawn(async () => {
		const socket = await connect();
		// The run may already be over here. cleanup() closes the socket right away if so.
		effect.cleanup(() => socket.close());
	});
});
```

A stale task may still want to bail for its own reasons, since work outside the effect's scope (plain fields, backoff counters) is not unwound for it. `close()` is permanent; reruns are not.

What makes this unconditional is that a rerun does not open the next run until every `spawn` task from the previous one has settled, however long that takes. So there is no window in which a slow task wakes up to find a different run installed: `connect()` can take a minute and its `cleanup` still fires immediately, and `effect.abort` still reads as this task's own aborted signal.

Teardown runs *before* that wait, so a task is not left hanging: whatever it was awaiting has already been closed, and it unwinds from there. The order is deliberate. A task that ignores both `effect.abort` and `effect.cancel` stalls the rerun rather than leaking its resources, and warns after 5 seconds in dev so the offender is named. `close()` is never blocked and releases everything.

A stale task may still want to bail early for its own reasons, which reads the same way it always did:

```ts
new Effect((effect) => {
	effect.spawn(async () => {
		const socket = await connect();

		// Still this run's signal, no matter how long connect() took.
		if (effect.abort.aborted) {
			socket.close();
			return;
		}

		effect.cleanup(() => socket.close());
	});
});
```

### Scoped helpers

Use these instead of raw timers and listeners so cleanup is automatic. Do **not** reach for `setTimeout`, `setInterval`, `requestAnimationFrame`, or `addEventListener` directly inside an effect.

| Helper | Does |
|---|---|
| `effect.timer(fn, ms)` | `setTimeout` that cancels on rerun or close. |
| `effect.interval(fn, ms)` | `setInterval` that cancels on rerun or close. |
| `effect.timeout(fn, ms)` | Runs `fn` as a nested effect, then closes that child after `ms`. |
| `effect.animate(fn)` | `requestAnimationFrame` that cancels on rerun or close. |
| `effect.event(target, type, fn)` | `addEventListener` removed via an `AbortSignal`. |
| `effect.subscribe(sig, fn)` | Runs `fn` with the current value now and on every change. |
| `effect.set(sig, value, cleanup)` | Sets a signal for this run, restoring `cleanup` afterwards. |
| `effect.proxy(dst, src)` | Copies `src` into `dst` and keeps it in sync. |
| `effect.computed(fn)` | A `Computed` closed with this effect. |
| `effect.run(fn)` | A nested effect closed with this effect. |
| `effect.spawn(fn)` | An async task that blocks the next rerun until it settles. |

```ts
const visible = new Signal(true);
const button = document.createElement("button");

new Effect((effect) => {
	if (!effect.get(visible)) return;

	effect.event(button, "click", () => console.log("clicked"));
	effect.interval(() => console.log("tick"), 1000);
	effect.timer(() => console.log("one second later"), 1000);
});
```

`effect.set` restores the cleanup value when the run ends. The cleanup argument is only optional when the signal's type already includes `undefined`:

```ts
const active = new Signal(false);

new Effect((effect) => {
	if (!effect.get(visible)) return;
	effect.set(active, true, false); // false again on rerun or close
});
```

### Nesting

`effect.run(fn)` creates a child scope that reruns independently and is closed with its parent. Prefer several small effects over one big one, so unrelated dependencies do not retrigger each other.

```ts
const broadcast = new Signal("demo");
const volume = new Signal(1);

new Effect((effect) => {
	console.log("broadcast:", effect.get(broadcast));

	// NOTE: use the nested effect's argument, not the parent's.
	effect.run((nested) => {
		console.log("volume:", nested.get(volume));
	});
});

volume.set(0.5); // only the nested effect reruns
```

`run` returns a disposer that closes the child early and releases it from the parent, so a long-lived effect spawning a child per event does not accumulate dead scopes:

```ts
const signals = new Effect();

const dispose = signals.run((effect) => {
	console.log("volume:", effect.get(volume));
});

dispose(); // closed now, not when `signals` closes
```

### Async

`effect.spawn` runs a task and blocks the next rerun until it settles, warning after 5 seconds in dev but continuing to wait. Three handles tell a task when its run is over:

- `effect.cancel`: a promise that resolves when the current run is torn down.
- `effect.abort`: an `AbortSignal` that fires at the same moment. Pass it to `fetch`, streams, anything that takes one.
- `effect.closed`: a promise that resolves on `close()` only, not on a rerun.

```ts
const endpoint = new Signal("/api/data");

new Effect((effect) => {
	const url = effect.get(endpoint);

	effect.spawn(async () => {
		const res = await fetch(url, { signal: effect.abort });
		console.log(await res.text());
	});
});
```

### Dev warnings

In development builds the library shouts when a lifecycle rule is broken. Each of these means a real bug, not noise:

- A signal passing roughly 100 subscribers throws `"signal has too many subscribers; may be leaking"`. Something is subscribing without disposing.
- An effect that tracked nothing warns `"Effect did not subscribe to any signals; it will never rerun."` You probably used `peek()` where you meant `effect.get()`.
- A `FinalizationRegistry` warns when an `Effect` is garbage collected without `close()`.

## Once and GetPromise

A `Once` settles exactly once and then never changes. It is both observable and awaitable, which makes it the shape for terminal state such as "closed": one handle serves the synchronous check, the reactive short-circuit, and the `await`.

Expose it to callers as `GetPromise<T>` so they can observe and await it but not settle it.

```ts
import { type GetPromise, Once } from "@moq/signals";

class Connection {
	#closed = new Once<Error | null>();

	/** Settles when the connection closes: `null` for a clean close, an `Error` for an abort. */
	get closed(): GetPromise<Error | null> {
		return this.#closed;
	}

	close(err?: Error): void {
		// Once.set throws on a second settle, so every close path guards and stays idempotent.
		if (this.#closed.peek() !== undefined) return;
		this.#closed.set(err ?? null);
	}
}
```

There are three states, and they need testing explicitly:

| `peek()` | Means |
|---|---|
| `undefined` | Still open. This is the pending sentinel, so `T` must not include `undefined`. |
| `null` | Closed cleanly. |
| `Error` | Aborted. |

**`if (closed)` means "aborted", not "closed".** Use `closed.peek() !== undefined` for "is it closed".

All three reads off the same handle:

```ts
import { Effect } from "@moq/signals";

const conn = new Connection();

// Synchronous check.
if (conn.closed.peek() !== undefined) console.log("already closed");

// Reactive: short-circuit an effect when it closes.
new Effect((effect) => {
	if (effect.get(conn.closed) !== undefined) return;
	// ... still open ...
});

// Awaited, resolving immediately if it already settled.
const err = await conn.closed;
if (err) console.warn("aborted:", err);
```

`Once` is a thenable, not a `Promise`, so use `.then()` rather than `.finally()` or `.catch()`. It never rejects on its own; an abort arrives as the resolved `Error` value.

## Components: `in`, `out`, and knobs

Every reactive component in `@moq/watch` and `@moq/publish` follows one shape, and yours should too if you are wiring them together.

- **`in`**: the wired dependencies, built in the constructor with `getter(...)` and exposed as `Readonlys<XxxInput>`.
- **`out`**: derived state, written privately and exposed through `readonlys(...)`. Never hand out a writable `Signal`, since that lets a caller forge state behind the owner's back.
- **Knobs**: live-editable settings the component does not derive. They stay public writable `Signal`s outside both groups.

`Inputs<I>` derives the constructor argument from the `in` map: every entry becomes optional and accepts a raw value, a `Signal`, or another component's `out` getter.

```ts
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

/** The signals a Ticker reads. */
export type TickerInput = {
	/** Whether the ticker is running. */
	enabled: Getter<boolean>;
};

type TickerOutput = {
	/** Ticks since the ticker last started. */
	count: Signal<number>;
};

/** Constructor options: the wired inputs plus the live-editable knobs. */
export type TickerProps = Inputs<TickerInput> & {
	/** Milliseconds between ticks. Also editable later via `ticker.interval`. */
	interval?: number | Signal<number>;
};

export class Ticker {
	readonly in: Readonlys<TickerInput>;

	/** Live-editable tick interval in milliseconds. */
	readonly interval: Signal<number>;

	readonly #out: TickerOutput = { count: new Signal(0) };
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: TickerProps) {
		this.in = { enabled: getter(props?.enabled ?? true) };
		this.interval = Signal.from(props?.interval ?? 1000);

		this.#signals.run((effect) => {
			if (!effect.get(this.in.enabled)) return;

			effect.set(this.#out.count, 0, 0);
			effect.interval(() => this.#out.count.update((n) => n + 1), effect.get(this.interval));
		});
	}

	/** Stops the ticker permanently. */
	close(): void {
		this.#signals.close();
	}
}
```

Wiring one component's `out` into another's `in` is then just a field, because `getter()` passes our readables through untouched:

```ts
const running = new Signal(true);
const ticker = new Ticker({ enabled: running, interval: 500 });

console.log(ticker.out.count.peek()); // read-only to us
ticker.interval.set(250); // knobs stay writable
```

`getter()` checks the brand, not the class, so a `Signal`, `Computed`, `Once`, or `Derived` from any copy of `@moq/signals` in the dependency tree passes straight through. What it rejects is an unbranded object that merely looks like one (`peek`, `subscribe`, and `changed` of your own), since wrapping that would silently freeze it into a constant that never updates. Anything else is treated as a plain value and wrapped in a fresh `Signal`.

The `#signals` effect stays private and `close()` is the only handle. The two custom elements (`<moq-watch>` and `<moq-publish>`) are the exception: they expose `readonly signals` as the documented place for an app to hang its own reactivity.

## Derived

A `Derived` is a read-only view over other readables, recomputed on every read. It is the lifecycle-free counterpart to `Computed`:

| | `Computed` | `Derived` |
|---|---|---|
| Dependencies | Tracked automatically via `effect.get` | Named up front |
| First read | `undefined` until the first run completes | Correct immediately |
| Value | Cached, refreshed on a microtask | Recomputed on every read |
| Teardown | Needs `close()` | None |
| Compute function | May be expensive | Must be cheap and pure |

Reach for it when a class wants to publish a small mapped view of its own state as part of its public surface. A hand-written object with the same three methods would work until a consumer passed it to `getter()` or an `Inputs` field, which reject a readable this package did not create.

```ts
import { Derived, Signal } from "@moq/signals";

class Room {
	#peers = new Signal(new Set<string>());

	/** True while at least one peer is connected. */
	readonly online = new Derived([this.#peers], (peers) => peers.size > 0);

	join(name: string): void {
		this.#peers.mutate((peers) => peers.add(name));
	}
}

const room = new Room();
room.online.peek(); // false, no first-run gap
room.join("alice");
room.online.peek(); // true
```

It notifies only when the derived value actually changes, matching `Signal`. A source can move without moving the view: another peer joining an already-online room changes `#peers` but not `online`, so subscribers stay quiet.

## Framework adapters

### React

```tsx
import { Signal } from "@moq/signals";
import { useSignal, useValue } from "@moq/signals/react";

const count = new Signal(0);

function Counter() {
	const value = useValue(count); // read-only
	const [other, setOther] = useSignal(count); // read-write, like useState

	return (
		<button type="button" onClick={() => setOther(other + 1)}>
			Count: {value}
		</button>
	);
}
```

`useValue` is a `useSyncExternalStore` subscription, so it works with concurrent rendering. `useSignal` accepts a value or an updater function, like `useState`.

### SolidJS

```ts
import { Signal } from "@moq/signals";
import { createAccessor, createPair, createSetter } from "@moq/signals/solid";

const count = new Signal(0);

const value = createAccessor(count); // Accessor<number>, unsubscribes on cleanup
const setValue = createSetter(count); // Setter<number>, writes through
const [get, set] = createPair(count); // both at once
```

### DOM

`@moq/signals/dom` builds elements and reactive content on top of an `Effect`, with cleanup handled for you. This is what the `@moq/watch` and `@moq/publish` UI components use.

```ts
import { Effect, Signal } from "@moq/signals";
import { create, render, setClass } from "@moq/signals/dom";

const label = new Signal("hello");
const highlight = new Signal(false);

const container = create("div", { className: "row" });

new Effect((effect) => {
	// Removed again when the effect reruns or closes.
	render(effect, container, effect.get(label));

	if (effect.get(highlight)) {
		setClass(effect, container, "highlight");
	}
});
```

## Usage with @moq/hang

Everything the media packages expose is a signal, so an app reads them the same way it reads its own state:

```ts
import "@moq/watch/element";

const watch = document.querySelector("moq-watch");
if (!watch) throw new Error("no <moq-watch> on the page");

// The element exposes its Effect so an app can hang reactivity off it.
watch.signals.run((effect) => {
	console.log("volume:", effect.get(watch.controls.volume));
	console.log("paused:", effect.get(watch.controls.paused));
});

watch.controls.volume.set(0.5);
```

Combine that with an adapter and the same signals drive a React or Solid tree:

```tsx
import { useValue } from "@moq/signals/react";
import type MoqWatch from "@moq/watch/element";

function Controls({ watch }: { watch: MoqWatch }) {
	const volume = useValue(watch.controls.volume);

	return (
		<input
			type="range"
			min="0"
			max="1"
			step="0.1"
			value={volume}
			onChange={(e) => watch.controls.volume.set(Number(e.target.value))}
		/>
	);
}
```

## Related Packages

- **[@moq/net](/lib/js/@moq/net)**: pub/sub transport, with `closed` state as `GetPromise`
- **[@moq/hang](/lib/js/@moq/hang/)**: media catalog and container
- **[@moq/watch](/lib/js/@moq/watch)** and **[@moq/publish](/lib/js/@moq/publish)**: the `in`/`out`/knobs convention in practice
- **[Web Components](/lib/js/env/web)**: the custom elements built on these signals
