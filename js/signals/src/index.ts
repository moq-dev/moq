/**
 * Reactive, safe signals: observable values, derived computeds, and effects
 * that track their dependencies and clean up automatically.
 *
 * @module
 */

/** Cancels a subscription, effect, or other registration when called. */
export type Dispose = () => void;

type Subscriber<T> = (value: T) => void;

// @ts-ignore - Some environments don't recognize import.meta.env
const DEV = typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";

// Symbol to identify Signal instances across different package versions
const SIGNAL_BRAND = Symbol.for("@moq/signals");

/** Read side of a signal: peek the current value and subscribe to changes. */
export interface Getter<T> {
	/** Returns the current value without subscribing. */
	peek(): T;

	/** Calls `fn` once the next time the value changes. */
	changed(fn: Subscriber<T>): Dispose;

	/** Calls `fn` every time the value changes. */
	subscribe(fn: Subscriber<T>): Dispose;
}

/** Write side of a signal: replace or transform the current value. */
export interface Setter<T> {
	/** Replaces the value, or transforms it via a function of the previous value. */
	set(value: T | ((prev: T) => T)): void;
	/** Transforms the value via a function of the previous value. */
	update(fn: (prev: T) => T): void;
}

/** A mutable observable value. Writes are coalesced per microtask and only notify subscribers when the value actually changes. */
export class Signal<T> implements Getter<T>, Setter<T> {
	#value: T;

	#subscribers: Set<Subscriber<T>> = new Set();
	#changed: Set<Subscriber<T>> = new Set();

	// Microtask coalescing state
	#pending = false;
	#oldValue: T | undefined;
	#hasCapturedOldValue = false;
	#forceNotify = false;

	// Brand to identify this as a Signal across package instances
	readonly [SIGNAL_BRAND] = true;

	constructor(value: T) {
		this.#value = value;
	}

	/** Returns the value if it's already a Signal, otherwise wraps it in a new Signal. */
	static from<T>(value: T | Signal<T>): Signal<T> {
		// Use brand check instead of instanceof to work across package instances
		if (typeof value === "object" && value !== null && SIGNAL_BRAND in value) {
			return value as Signal<T>;
		}
		return new Signal(value);
	}

	/** Returns the current value without subscribing. */
	get(): T {
		return this.#value;
	}

	/** Returns the current value without subscribing. */
	// TODO rename to `get` once we've ported everything
	peek(): T {
		return this.#value;
	}

	/**
	 * Sets the current value, notifying subscribers if it changed.
	 * Pass `notify` true to always notify or false to never notify.
	 */
	set(value: T, notify?: boolean): void {
		// Capture old value before the first set in this microtask.
		if (!this.#hasCapturedOldValue) {
			this.#oldValue = this.#value;
			this.#hasCapturedOldValue = true;
		}

		this.#value = value;

		// If notify is false, don't notify.
		if (notify === false) return;

		if (notify === true) this.#forceNotify = true;

		// If there are no subscribers, don't queue a microtask.
		// Reset all pending state since no flush will occur to clear it.
		if (this.#subscribers.size === 0 && this.#changed.size === 0) {
			this.#hasCapturedOldValue = false;
			this.#oldValue = undefined;
			this.#forceNotify = false;
			return;
		}

		// Coalesce multiple set() calls into a single microtask.
		if (this.#pending) return;
		this.#pending = true;

		queueMicrotask(() => this.#flush());
	}

	#flush(): void {
		this.#pending = false;
		this.#hasCapturedOldValue = false;
		const old = this.#oldValue;
		this.#oldValue = undefined;

		const force = this.#forceNotify;
		this.#forceNotify = false;

		// Check if the net change is zero (value returned to what it was before).
		// Use === for class instances, dequal for plain objects/primitives.
		if (!force && isEqual(old as T, this.#value)) return;

		const value = this.#value;
		const changed = this.#changed;
		this.#changed = new Set();

		for (const fn of this.#subscribers) {
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
	}

	/** Sets the value to the result of `fn(prev)`, notifying subscribers unless `notify` is false. */
	update(fn: (prev: T) => T, notify = true): void {
		const value = fn(this.#value);
		this.set(value, notify);
	}

	/**
	 * Mutates the current value in place via `fn`, returning `fn`'s result and
	 * notifying subscribers unless `notify` is false.
	 */
	mutate<R>(fn: (value: T) => R, notify = true): R {
		const r = fn(this.#value);
		this.set(this.#value, notify);
		return r;
	}

	/** Calls `fn` every time the value changes. Returns a function to unsubscribe. */
	subscribe(fn: Subscriber<T>): Dispose {
		this.#subscribers.add(fn);
		if (DEV && this.#subscribers.size >= 100 && Number.isInteger(Math.log10(this.#subscribers.size))) {
			throw new Error("signal has too many subscribers; may be leaking");
		}
		return () => this.#subscribers.delete(fn);
	}

	/** Calls `fn` once the next time the value changes. Returns a function to cancel. */
	changed(fn: (value: T) => void): Dispose {
		this.#changed.add(fn);
		return () => this.#changed.delete(fn);
	}

	/** Resolves with the next value, once the signal changes. */
	next(): Promise<T> {
		return new Promise<T>((resolve) => {
			this.changed(resolve);
		});
	}

	/** Calls `fn` with the current value now, and again every time it changes. */
	watch(fn: Subscriber<T>): Dispose {
		const dispose = this.subscribe(fn);
		queueMicrotask(() => fn(this.#value));
		return dispose;
	}

	/** Resolves with the next value from whichever of the given signals changes first. */
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

/**
 * Runs a function that reads signals via `effect.get(...)` and reruns whenever
 * any of them change. Registers cleanup, timers, and event listeners that are
 * torn down automatically on each rerun and when the effect is closed.
 */
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

	#stopped: PromiseWithResolvers<void>;
	#closed: PromiseWithResolvers<void>;

	#abort: AbortController = new AbortController();

	/** If a function is provided, it runs immediately and reruns whenever a tracked signal changes. */
	constructor(fn?: (effect: Effect) => void) {
		if (DEV) {
			const debug = new Error("created here:").stack ?? "No stack";
			Effect.#finalizer.register(this, debug, this);
		}

		this.#fn = fn;

		if (DEV) {
			this.#stack = new Error().stack;
		}

		this.#stopped = Promise.withResolvers();
		this.#closed = Promise.withResolvers();

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

		this.#stopped.resolve();
		this.#abort.abort();
		this.#abort = new AbortController();

		this.#stopped = Promise.withResolvers();

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

			if (
				DEV &&
				this.#dispose !== undefined &&
				this.#unwatch.length === 0 &&
				this.#dispose.length === 0 &&
				this.#async.length === 0
			) {
				console.warn("Effect did not subscribe to any signals; it will never rerun.", this.#stack);
			}
		}
	}

	/** Reads a signal and tracks it, rerunning the effect whenever it changes. */
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

	/**
	 * Sets a signal for the duration of this run, restoring `cleanup` on rerun or close.
	 * The cleanup value is optional only when the signal type includes `undefined`.
	 */
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

	/**
	 * Runs an async task. The effect will not rerun until the task's promise settles.
	 */
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

	/** Runs `fn` after `ms` milliseconds, unless the effect reruns or closes first. */
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

	/** Runs `fn` as a nested effect, then closes that effect after `ms` milliseconds. */
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

	/** Runs `fn` on the next animation frame, unless the effect reruns or closes first. */
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

	/** Runs `fn` every `ms` milliseconds until the effect reruns or closes. */
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

	/** Creates a nested effect that reruns independently and is closed with its parent. */
	run(fn: (effect: Effect) => void) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.nested called when closed, ignoring");
			}
			return;
		}

		const effect = new Effect(fn);
		this.#dispose.push(() => effect.close());
	}

	/** Alias for {@link run}, kept for backwards compatibility. */
	effect(fn: (effect: Effect) => void) {
		return this.run(fn);
	}

	/** Creates a derived signal scoped to this effect, closed when the effect reruns or closes. */
	computed<T>(fn: (effect: Effect) => T): Computed<T> {
		const computed = new Computed(fn);
		this.cleanup(() => computed.close());
		return computed;
	}

	/** Reads and tracks several signals, returning their values or `undefined` if any is falsy. */
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

	/** Runs `fn` with the signal's value now and again whenever it changes, scoped to this effect. */
	subscribe<T>(signal: Getter<T>, fn: (value: T) => void) {
		if (this.#dispose === undefined) {
			if (DEV) {
				console.warn("Effect.subscribe called when closed, running once");
			}
			fn(signal.peek());
			return;
		}

		this.run((effect) => {
			const value = effect.get(signal);
			fn(value);
		});
	}

	/** Adds an event listener that is removed automatically when the effect reruns or closes. */
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

		// Merge the abort signal so the listener is auto-removed on rerun/close.
		const signal =
			typeof options !== "boolean" && options?.signal
				? AbortSignal.any([this.#abort.signal, options.signal])
				: this.#abort.signal;
		const merged: AddEventListenerOptions =
			typeof options === "boolean" ? { capture: options, signal } : { ...options, signal };

		target.addEventListener(type, listener, merged);
	}

	/** Registers a function to run when the effect reruns or closes. */
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

	/** Stops the effect permanently, running all cleanup and unsubscribing from every signal. */
	close(): void {
		if (this.#dispose === undefined) {
			return;
		}

		this.#closed.resolve();
		this.#stopped.resolve();
		this.#abort.abort();

		for (const fn of this.#dispose) fn();
		this.#dispose = undefined;

		for (const signal of this.#unwatch) signal();
		this.#unwatch.length = 0;

		this.#async.length = 0;

		if (DEV) {
			Effect.#finalizer.unregister(this);
		}
	}

	/** Resolves when the effect is closed. */
	get closed(): Promise<void> {
		return this.#closed.promise;
	}

	/** Resolves when the current run is about to be torn down, by a rerun or close. */
	get cancel(): Promise<void> {
		return this.#stopped.promise;
	}

	/** An AbortSignal that fires when the current run is torn down. */
	get abort(): AbortSignal {
		return this.#abort.signal;
	}

	/** Copies `src` into `dst` and keeps `dst` in sync as `src` changes. */
	proxy<T>(dst: Setter<T>, src: Getter<T>): void {
		this.subscribe(src, (value) => dst.update(() => value));
	}
}

/**
 * A read-only signal derived from other signals.
 *
 * The compute function reads its dependencies with `effect.get(...)`, exactly
 * like an effect, and returns the derived value. It reruns whenever a
 * dependency changes. Keep it pure: derive a value, don't perform side effects.
 *
 * Like every signal, updates are asynchronous: the value is `undefined` until
 * the first run completes (and after close()), and recomputes propagate on a
 * microtask. Read it inside an effect and handle the `undefined` case, the same
 * way you would any other signal that starts empty.
 */
export class Computed<T> implements Getter<T | undefined> {
	#signal = new Signal<T | undefined>(undefined);
	#effect: Effect;

	/** Creates a computed that derives its value from `fn`, rerunning when dependencies change. */
	constructor(fn: (effect: Effect) => T) {
		this.#effect = new Effect((effect) => {
			this.#signal.set(fn(effect));
		});
	}

	/** Returns the current derived value without subscribing (`undefined` until the first run). */
	peek(): T | undefined {
		return this.#signal.peek();
	}

	/** Calls `fn` once the next time the derived value changes. */
	changed(fn: Subscriber<T | undefined>): Dispose {
		return this.#signal.changed(fn);
	}

	/** Calls `fn` every time the derived value changes. */
	subscribe(fn: Subscriber<T | undefined>): Dispose {
		return this.#signal.subscribe(fn);
	}

	/**
	 * Stops recomputing and tracking dependencies. Required for standalone computeds;
	 * an `effect.computed()` is closed automatically with its parent effect.
	 */
	close(): void {
		this.#effect.close();
	}
}

// Deep equality for plain objects/arrays, === for class instances and primitives.
// Class instances have identity semantics (e.g. two different Broadcast instances are never equal).
function isEqual(a: unknown, b: unknown): boolean {
	if (a === b) return true;
	if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;

	const protoA = Object.getPrototypeOf(a);
	const protoB = Object.getPrototypeOf(b);

	// Both must be plain objects or both arrays to deep-compare.
	if (protoA !== protoB) return false;
	if (protoA !== Object.prototype && protoA !== Array.prototype) return false;

	const keysA = Object.keys(a as Record<string, unknown>);
	const keysB = Object.keys(b as Record<string, unknown>);
	if (keysA.length !== keysB.length) return false;

	for (const key of keysA) {
		if (!isEqual((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key])) return false;
	}

	return true;
}
