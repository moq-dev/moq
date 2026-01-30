import { dequal } from "dequal";

export type Dispose = () => void;

type Subscriber<T> = (value: T) => void;

// @ts-ignore - Some environments don't recognize import.meta.env
const DEV = typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";

// Symbol to identify Signal instances across different package versions
const SIGNAL_BRAND = Symbol.for("@moq/signals");

export interface Getter<T> {
	// Get the current value.
	peek(): T;

	// Receive a notification once when the value changes.
	changed(fn: Subscriber<T>): Dispose;

	// Receive a notification each time the value changes.
	subscribe(fn: Subscriber<T>): Dispose;
}

export interface Setter<T> {
	set(value: T | ((prev: T) => T)): void;
}

export class Signal<T> implements Getter<T>, Setter<T> {
	#value: T;

	#subscribers: Set<Subscriber<T>> = new Set();
	#changed: Set<Subscriber<T>> = new Set();

	// Brand to identify this as a Signal across package instances
	readonly [SIGNAL_BRAND] = true;

	constructor(value: T) {
		this.#value = value;
	}

	static from<T>(value: T | Signal<T>): Signal<T> {
		// Use brand check instead of instanceof to work across package instances
		if (typeof value === "object" && value !== null && SIGNAL_BRAND in value) {
			return value as Signal<T>;
		}
		return new Signal(value);
	}

	get(): T {
		return this.#value;
	}

	// TODO rename to `get` once we've ported everything
	peek(): T {
		return this.#value;
	}

	// Set the current value, by default notifying subscribers if the value is different.
	// If notify is undefined, we'll check if the value has changed after the microtask.
	set(value: T, notify?: boolean): void {
		const old = this.#value;
		this.#value = value;

		// If notify is false, don't notify.
		if (notify === false) return;

		// Don't even queue a microtask if the value is the EXACT same.
		// We don't use dequal here because we don't want to run it twice, only when it matters.
		if (notify === undefined && old === this.#value) return;

		// If there are no subscribers, don't queue a microtask.
		if (this.#subscribers.size === 0 && this.#changed.size === 0) return;

		const subscribers = this.#subscribers;
		const changed = this.#changed;
		this.#changed = new Set();

		queueMicrotask(() => {
			// After the microtask, check if the value has changed if we didn't explicitly notify.
			if (notify === undefined && dequal(old, this.#value)) {
				// No change, add back the changed subscribers.
				for (const fn of changed) {
					this.#changed.add(fn);
				}
				return;
			}

			for (const fn of subscribers) {
				try {
					fn(value);
				} catch (error) {
					console.error("signal subscriber error", error);
				}
			}

			for (const fn of changed) {
				try {
					fn(value);
				} catch (error) {
					console.error("signal changed error", error);
				}
			}
		});
	}

	// Mutate the current value and notify subscribers unless notify is false.
	// Unlike set, we can't use a dequal check because the function may mutate the value.
	update(fn: (prev: T) => T, notify = true): void {
		const value = fn(this.#value);
		this.set(value, notify);
	}

	// Mutate the current value and notify subscribers unless notify is false.
	mutate<R>(fn: (value: T) => R, notify = true): R {
		const r = fn(this.#value);
		this.set(this.#value, notify);
		return r;
	}

	// Receive a notification each time the value changes.
	subscribe(fn: Subscriber<T>): Dispose {
		this.#subscribers.add(fn);
		if (DEV && this.#subscribers.size >= 100 && Number.isInteger(Math.log10(this.#subscribers.size))) {
			throw new Error("signal has too many subscribers; may be leaking");
		}
		return () => this.#subscribers.delete(fn);
	}

	// Receive a notification when the value changes.
	changed(fn: (value: T) => void): Dispose {
		this.#changed.add(fn);
		return () => this.#changed.delete(fn);
	}

	// Receive a notification when the value changes AND with the initial value.
	watch(fn: Subscriber<T>): Dispose {
		const dispose = this.subscribe(fn);
		queueMicrotask(() => fn(this.#value));
		return dispose;
	}

	static async race<T extends readonly unknown[]>(
		...sigs: { [K in keyof T]: Signal<T[K]> }
	): Promise<Awaited<T[number]>> {
		const dispose: Dispose[] = [];

		const result: Awaited<T[number]> = await new Promise((resolve) => {
			for (const sig of sigs) {
				dispose.push(sig.changed(resolve));
			}
		});

		for (const fn of dispose) fn();
		return result;
	}
}

type SetterType<S> = S extends Setter<infer T> ? T : never;
type GetterType<G> = G extends Getter<infer T> ? T : never;

// Excludes common falsy values from a type
type Falsy = false | 0 | "" | null | undefined;
type Truthy<T> = Exclude<T, Falsy>;

// TODO Make this a single instance of an Effect, so close() can work correctly from async code.
export class Effect {
	// Sanity check to make sure roots are being disposed on dev.
	static #finalizer = new FinalizationRegistry<string>((debugInfo) => {
		console.warn(`Signals was garbage collected without being closed:\n${debugInfo}`);
	});

	#fn?: (effect: Effect) => void;
	#dispose?: Dispose[] = [];
	#unwatch: Dispose[] = [];
	#async: Promise<void>[] = [];

	#stack?: string;
	#scheduled = false;

	#stop!: () => void;
	#stopped: Promise<void>;

	#close!: () => void;
	#closed: Promise<void>;

	// If a function is provided, it will be run with the effect as an argument.
	constructor(fn?: (effect: Effect) => void) {
		if (DEV) {
			const debug = new Error("created here:").stack ?? "No stack";
			Effect.#finalizer.register(this, debug, this);
		}

		this.#fn = fn;

		if (DEV) {
			this.#stack = new Error().stack;
		}

		this.#stopped = new Promise((resolve) => {
			this.#stop = resolve;
		});

		this.#closed = new Promise((resolve) => {
			this.#close = resolve;
		});

		if (fn) {
			this.#schedule();
		}
	}

	#schedule(): void {
		if (this.#scheduled) return;
		this.#scheduled = true;

		// We always queue a microtask to make it more difficult to get stuck in an infinite loop.
		queueMicrotask(() =>
			this.#run().catch((error) => {
				console.error("effect error", error, this.#stack);
			}),
		);
	}

	async #run(): Promise<void> {
		if (this.#dispose === undefined) return; // closed, no error because this is a microtask

		this.#stop();
		this.#stopped = new Promise((resolve) => {
			this.#stop = resolve;
		});

		// Unsubscribe from all signals.
		for (const unwatch of this.#unwatch) unwatch();
		this.#unwatch.length = 0;

		// Run the cleanup functions for the previous run.
		for (const fn of this.#dispose) fn();
		this.#dispose.length = 0;

		// Wait for all async effects to complete.
		if (this.#async.length > 0) {
			try {
				let warn: ReturnType<typeof setTimeout> | undefined;
				const timeout = new Promise<void>((resolve) => {
					warn = setTimeout(() => {
						if (DEV) {
							console.warn("spawn is still running after 5s; continuing anyway", this.#stack);
						}

						resolve();
					}, 5000);
				});

				await Promise.race([Promise.all(this.#async), timeout]);
				if (warn) clearTimeout(warn);

				this.#async.length = 0;
			} catch (error) {
				console.error("async effect error", error);
				if (this.#stack) console.error("stack", this.#stack);
			}
		}

		// We were closed while waiting for async effects to complete.
		if (this.#dispose === undefined) return;

		// IMPORTANT: must run all of the dispose functions before unscheduling.
		// Otherwise, cleanup functions could get us stuck in an infinite loop.
		this.#scheduled = false;

		if (this.#fn) {
			this.#fn(this);
		}
	}

	// Get the current value of a signal, monitoring it for changes (via ===) and rerunning on change.
	get<T>(signal: Getter<T>): T {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.get called when closed, returning current value");
			}
			return signal.peek();
		}

		const value = signal.peek();

		// NOTE: We use changed instead of subscribe just so it's slightly more efficient.
		// 1 clear() instead of N delete() calls.
		const dispose = signal.changed(() => this.#schedule());
		this.#unwatch.push(dispose);

		return value;
	}

	// Temporarily set the value of a signal, unsetting it on cleanup.
	// The last argument is the cleanup value, set before the effect is rerun.
	// It's optional only if T can be undefined.
	set<S extends Setter<unknown>>(
		signal: S,
		value: SetterType<S>,
		...args: undefined extends SetterType<S> ? [cleanup?: SetterType<S>] : [cleanup: SetterType<S>]
	): void {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.set called when closed, ignoring");
			}
			return;
		}

		signal.set(value);
		const cleanup = args[0];
		const cleanupValue = cleanup === undefined ? (undefined as SetterType<S>) : cleanup;
		this.cleanup(() => signal.set(cleanupValue));
	}

	// Spawn an async effect that blocks the effect being reloaded until it completes.
	// Use this.cancel if you need to detect when the effect is reloading to terminate.
	// TODO: Add effect for another layer of nesting
	spawn(fn: () => Promise<void>) {
		const promise = fn().catch((error) => {
			console.error("spawn error", error);
		});

		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.spawn called when closed");
			}

			return;
		}

		this.#async.push(promise);
	}

	// Run the function after the given delay in milliseconds UNLESS the effect is cleaned up first.
	timer(fn: () => void, ms: DOMHighResTimeStamp) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.timer called when closed, ignoring");
			}
			return;
		}

		let timeout: ReturnType<typeof setTimeout> | undefined;
		timeout = setTimeout(() => {
			timeout = undefined;
			fn();
		}, ms);
		this.cleanup(() => timeout && clearTimeout(timeout));
	}

	// Run the function, and clean up the nested effect after the given delay.
	timeout(fn: (effect: Effect) => void, ms: DOMHighResTimeStamp) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.timeout called when closed, ignoring");
			}
			return;
		}

		const effect = new Effect(fn);

		let timeout: ReturnType<typeof setTimeout> | undefined = setTimeout(() => {
			effect.close();
			timeout = undefined;
		}, ms);

		this.#dispose.push(() => {
			if (timeout) {
				clearTimeout(timeout);
				effect.close();
			}
		});
	}

	// Run the callback on the next animation frame, unless the effect is cleaned up first.
	animate(fn: (now: DOMHighResTimeStamp) => void) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.animate called when closed, ignoring");
			}
			return;
		}

		let animate: number | undefined = requestAnimationFrame((now) => {
			fn(now);
			animate = undefined;
		});
		this.cleanup(() => {
			if (animate) cancelAnimationFrame(animate);
		});
	}

	interval(fn: () => void, ms: DOMHighResTimeStamp) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.interval called when closed, ignoring");
			}
			return;
		}

		const interval = setInterval(() => {
			fn();
		}, ms);
		this.cleanup(() => clearInterval(interval));
	}

	// Create a nested effect that can be rerun independently.
	effect(fn: (effect: Effect) => void) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.nested called when closed, ignoring");
			}
			return;
		}

		const effect = new Effect(fn);
		this.#dispose.push(() => effect.close());
	}

	// Get the values of multiple signals, returning undefined if any are falsy.
	getAll<S extends readonly Getter<unknown>[]>(
		signals: [...S],
	): { [K in keyof S]: Truthy<GetterType<S[K]>> } | undefined {
		const values: unknown[] = [];
		for (const signal of signals) {
			const value = this.get(signal);
			if (!value) return undefined;
			values.push(value);
		}
		return values as { [K in keyof S]: Truthy<GetterType<S[K]>> };
	}

	// A helper to call a function when a signal changes.
	subscribe<T>(signal: Getter<T>, fn: (value: T) => void) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.subscribe called when closed, running once");
			}
			fn(signal.peek());
			return;
		}

		this.effect((effect) => {
			const value = effect.get(signal);
			fn(value);
		});
	}

	// Add an event listener that automatically removes on cleanup.
	event<K extends keyof HTMLElementEventMap>(
		target: HTMLElement,
		type: K,
		listener: (this: HTMLElement, ev: HTMLElementEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof SVGElementEventMap>(
		target: SVGElement,
		type: K,
		listener: (this: SVGElement, ev: SVGElementEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof DocumentEventMap>(
		target: Document,
		type: K,
		listener: (this: Document, ev: DocumentEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof WindowEventMap>(
		target: Window,
		type: K,
		listener: (this: Window, ev: WindowEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof WebSocketEventMap>(
		target: WebSocket,
		type: K,
		listener: (this: WebSocket, ev: WebSocketEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof XMLHttpRequestEventMap>(
		target: XMLHttpRequest,
		type: K,
		listener: (this: XMLHttpRequest, ev: XMLHttpRequestEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof MediaQueryListEventMap>(
		target: MediaQueryList,
		type: K,
		listener: (this: MediaQueryList, ev: MediaQueryListEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof AnimationEventMap>(
		target: Animation,
		type: K,
		listener: (this: Animation, ev: AnimationEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event<K extends keyof EventSourceEventMap>(
		target: EventSource,
		type: K,
		listener: (this: EventSource, ev: EventSourceEventMap[K]) => void,
		options?: boolean | AddEventListenerOptions,
	): void;
	event(
		target: EventTarget,
		type: string,
		listener: EventListenerOrEventListenerObject,
		options?: boolean | AddEventListenerOptions,
	): void;
	event(
		target: EventTarget,
		type: string,
		listener: EventListenerOrEventListenerObject,
		options?: boolean | AddEventListenerOptions,
	): void {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.eventListener called when closed, ignoring");
			}
			return;
		}

		target.addEventListener(type, listener, options);
		this.cleanup(() => target.removeEventListener(type, listener, options));
	}

	// Reschedule the effect to run again.
	reload() {
		this.#schedule();
	}

	// Register a cleanup function.
	cleanup(fn: Dispose): void {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.cleanup called when closed, running immediately");
			}

			fn();
			return;
		}

		this.#dispose.push(fn);
	}

	close(): void {
		if (this.#dispose === undefined) {
			return;
		}

		this.#close();
		this.#stop();

		for (const fn of this.#dispose) fn();
		this.#dispose = undefined;

		for (const signal of this.#unwatch) signal();
		this.#unwatch.length = 0;

		this.#async.length = 0;

		if (DEV) {
			Effect.#finalizer.unregister(this);
		}
	}

	get closed(): Promise<void> {
		return this.#closed;
	}

	get cancel(): Promise<void> {
		return this.#stopped;
	}
}
