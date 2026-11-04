import { Milli, Nano, sleep } from "@moq/clock";
import { race } from "@moq/signals";
import { controlTimeout, error } from "../error.ts";
import type { Stream } from "../stream.ts";
import { TimeoutError } from "./timeout.ts";

/** Milliseconds a request may go unanswered once none of its streams waits to open. @internal */
export const REQUEST_TIMEOUT_MS = 10_000;

/** What a deadline failure resets: a stream, or whatever owns one. @internal */
export interface Abortable {
	abort(err: Error): void;
}

/**
 * One fatal deadline for a request, including every stream its setup creates.
 *
 * An open waiting for the peer's stream credit is waiting its turn, not the peer stalling, so
 * the deadline only runs while none of the request's streams waits to open. @internal
 */
export class Deadline {
	/** Creates still waiting on credit: a fetch opens its TRACK stream alongside. */
	#opening = 0;
	/** Settles whenever an open starts or ends, restarting the budget. */
	#changed!: Promise<void>;
	#notify!: () => void;
	#owners: Abortable[] = [];
	#failure?: Error;

	constructor() {
		this.#rearm();
	}

	#rearm() {
		this.#changed = new Promise((resolve) => {
			this.#notify = resolve;
		});
	}

	#change(delta: number) {
		this.#opening += delta;
		this.#notify();
		this.#rearm();
	}

	/**
	 * Record a create so deadline failures reset its stream even if it opens late. `own` wraps
	 * the opened stream in its owner, which the failure then resets instead.
	 */
	open(opening: Stream | Promise<Stream>): Promise<Stream>;
	open<T extends Abortable>(opening: Stream | Promise<Stream>, own: (stream: Stream) => T): Promise<T>;
	async open(
		opening: Stream | Promise<Stream>,
		own: (stream: Stream) => Abortable = (stream) => stream,
	): Promise<Abortable> {
		this.#change(1);
		let stream: Stream;
		try {
			stream = await opening;
		} finally {
			this.#change(-1);
		}
		const owner = own(stream);
		if (this.#failure) {
			owner.abort(this.#failure);
			throw this.#failure;
		}
		this.#owners.push(owner);
		return owner;
	}

	/** Write the request on an opened stream, which a deadline failure after resets. */
	async send(stream: Abortable, write: () => Promise<void>): Promise<void> {
		if (this.#failure) throw this.#failure;
		if (!this.#owners.includes(stream)) throw new Error("request sent on a stream the deadline did not open");
		await write();
	}

	/** Wait for the first answer under the request's single deadline. */
	async wait<T>(answer: Promise<T>): Promise<T> {
		const done = new AbortController();
		try {
			return (await race([answer, this.#expire(done.signal)])) as T;
		} catch (cause) {
			const failure = cause instanceof TimeoutError ? controlTimeout(cause) : error(cause);
			this.#failure = failure;
			// Other failures keep the caller's cancellation ordering.
			if (cause instanceof TimeoutError) {
				for (const owner of this.#owners) owner.abort(failure);
			}
			throw failure;
		} finally {
			done.abort();
		}
	}

	/** Reject once the request goes the whole budget with none of its streams waiting to open. */
	async #expire(done: AbortSignal): Promise<never> {
		const budget = Nano.fromMilli(Milli(REQUEST_TIMEOUT_MS));
		const answered = new Promise<void>((resolve) => done.addEventListener("abort", () => resolve(), { once: true }));
		for (;;) {
			const changed = this.#changed;
			const restart = new AbortController();
			const timer =
				this.#opening > 0
					? new Promise<void>(() => {})
					: sleep(budget, { signal: AbortSignal.any([restart.signal, done]) }).catch(() => undefined);
			const why = await race([timer.then(() => "expired" as const), changed.then(() => "changed" as const), answered]);
			restart.abort();
			// Answered: the waiting is over, and nothing is left to reject.
			if (why === undefined) return new Promise<never>(() => {});
			if (why === "expired" && !done.aborted) {
				throw new TimeoutError(`request timed out after ${REQUEST_TIMEOUT_MS}ms`);
			}
		}
	}
}
