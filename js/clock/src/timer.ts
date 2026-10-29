import { backend } from "./ambient.ts";
import type { Backend } from "./backend.ts";
import { Instant, type Nano } from "./types.ts";

function aborted(): DOMException {
	return new DOMException("timer closed", "AbortError");
}

// `ref: false` lets a Node or Bun process exit while only this timer is pending; browsers ignore it.
type Options = { signal?: AbortSignal; ref?: boolean };

function wait(clock: Backend, at: Instant, options: Options = {}): { promise: Promise<void>; cancel: () => void } {
	const { signal, ref = true } = options;
	let cancel = () => {};
	const promise = new Promise<void>((resolve, reject) => {
		let done = false;
		let disarm = () => {};
		const finish = (reason?: unknown) => {
			if (done) return;
			done = true;
			disarm();
			if (!signal?.aborted) signal?.removeEventListener("abort", abort);
			if (reason !== undefined) reject(reason);
			else resolve();
		};
		const abort = () => finish(signal?.reason ?? aborted());
		cancel = () => finish(aborted());
		if (signal?.aborted) return abort();
		signal?.addEventListener("abort", abort, { once: true });
		disarm = clock.arm(at, () => finish(), ref);
		if (done) disarm();
	});
	return { promise, cancel };
}

/** Read the installed clock's monotonic instant. */
export function now(): Instant {
	return Instant(backend().now());
}

/** Complete when the installed clock reaches an instant, or reject on abort; `ref: false` won't hold the process open. */
export function sleepUntil(at: Instant, options?: { signal?: AbortSignal; ref?: boolean }): Promise<void> {
	return wait(backend(), Instant(at), options).promise;
}

/** Complete after a duration on the installed clock, or reject on abort; `ref: false` won't hold the process open. */
export function sleep(duration: Nano, options?: { signal?: AbortSignal; ref?: boolean }): Promise<void> {
	const clock = backend();
	return wait(clock, Instant.add(Instant(clock.now()), duration), options).promise;
}

/** Complete in a later host task, giving the host's other work a turn; a `Manual` clock won't advance past it. */
export function yieldNow(): Promise<void> {
	const clock = backend();
	return new Promise((resolve) => clock.task(resolve));
}

/** A timeout expired before its operation completed. */
export class Elapsed extends Error {
	/** Build a deadline-elapsed error. */
	constructor() {
		super("deadline elapsed");
		this.name = "Elapsed";
	}
}

/** Resolve an operation within a duration, preferring an already-ready operation. */
export function timeout<T>(duration: Nano, operation: PromiseLike<T>): Promise<T> {
	const clock = backend();
	const timer = wait(clock, Instant.add(Instant(clock.now()), duration));
	return new Promise((resolve, reject) => {
		let done = false;
		const finish = (ok: boolean, value: unknown) => {
			if (done) return;
			done = true;
			timer.cancel();
			if (ok) resolve(value as T);
			else reject(value);
		};
		void Promise.resolve(operation).then(
			(value) => finish(true, value),
			(error) => finish(false, error),
		);
		void timer.promise.then(
			() => finish(false, new Elapsed()),
			() => {},
		);
	});
}

/** Choose how an interval catches up after its caller misses ticks. */
export const MissedTickBehavior = {
	/** Deliver every missed tick on the original schedule. */
	Burst: "burst",
	/** Deliver one missed tick, then wait a full period from now. */
	Delay: "delay",
	/** Deliver one missed tick, then resume the next original deadline. */
	Skip: "skip",
} as const;

/** The supported interval catch-up policies. */
export type MissedTickBehavior = (typeof MissedTickBehavior)[keyof typeof MissedTickBehavior];

/** An owned repeating timer, bound to the clock it was created on. */
export class Interval {
	#clock = backend();
	#next = Instant(this.#clock.now());
	#period: Nano;
	#missed: MissedTickBehavior = MissedTickBehavior.Burst;
	#abort = new AbortController();
	#pending = false;

	/** Start a positive-period interval whose first tick is ready at once. */
	constructor(period: Nano) {
		Instant.add(Instant.zero, period);
		if (period === 0) throw new RangeError("interval period must be positive");
		this.#period = period;
	}

	/** Wait for the next tick and return its scheduled instant; only one tick may wait at once. */
	async tick(): Promise<Instant> {
		if (this.#pending) throw new Error("interval tick already pending");
		this.#pending = true;
		try {
			await wait(this.#clock, this.#next, { signal: this.#abort.signal }).promise;
			const scheduled = this.#next;
			const now = this.#clock.now();
			let next = Instant.add(scheduled, this.#period);
			if (next <= now) {
				switch (this.#missed) {
					case MissedTickBehavior.Delay:
						next = Instant(now + this.#period);
						break;
					case MissedTickBehavior.Skip:
						next = Instant(now + this.#period - ((now - scheduled) % this.#period));
						break;
				}
			}
			this.#next = next;
			return scheduled;
		} finally {
			this.#pending = false;
		}
	}

	/** Set the catch-up policy, which defaults to Burst. */
	setMissedTickBehavior(behavior: MissedTickBehavior): void {
		if (!Object.values(MissedTickBehavior).includes(behavior)) throw new RangeError("invalid missed tick behavior");
		this.#missed = behavior;
	}

	/** Return the time between ticks. */
	period(): Nano {
		return this.#period;
	}

	/** Cancel the pending tick and reject all future ticks; safe to call more than once. */
	close(): void {
		this.#abort.abort(aborted());
	}
}

/** Tick every positive period on the installed clock, starting at once. */
export function interval(period: Nano): Interval {
	return new Interval(period);
}
