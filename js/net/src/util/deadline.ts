import { type Milli, Nano } from "@moq/clock";
import { controlTimeout, error, NoStreamCredit } from "../error.ts";
import type { OpenOptions, Stream } from "../stream.ts";
import { TimeoutError, withTimeout } from "./timeout.ts";

/** Request opens defer their deadline to the create-to-answer budget. @internal */
export const requestOptions = { timeout: null, waitUntilAvailable: false } satisfies Partial<OpenOptions>;
/** Milliseconds a discovery or advertisement request allows from its create to its first answer. @internal */
export const REQUEST_TIMEOUT_MS = 10_000;

// A closed session fails its creates with a session-sourced WebTransportError, or with an
// InvalidStateError once it has closed. Those report the session's failure, not missing credit.
function sessionFailure(err: unknown): boolean {
	if (typeof err !== "object" || err === null) return false;
	const { source, name } = err as { source?: unknown; name?: unknown };
	return source === "session" || name === "InvalidStateError";
}

/** What a deadline failure resets: a stream, or whatever owns one. @internal */
export interface Abortable {
	abort(err: Error): void;
}

/**
 * A request's setup: owns every stream it creates, fails a create without credit at once, and
 * fails the answer once `budget` passes, if given. A track request takes no budget, since the
 * publisher's application may hold its answer: demand, cancellation, and the session end it,
 * as in Rust. @internal
 */
export class Deadline {
	/** Creates still waiting on credit: a fetch opens its TRACK stream alongside. */
	#opening = 0;
	#owners: Abortable[] = [];
	#failure?: Error;
	#budget?: Milli;

	constructor(budget?: Milli) {
		this.#budget = budget;
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
		this.#opening++;
		let stream: Stream;
		try {
			stream = await opening;
		} catch (cause) {
			if (sessionFailure(cause)) throw cause;
			throw new NoStreamCredit({ cause });
		} finally {
			this.#opening--;
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

	/** Wait for the first answer, within the budget if there is one. */
	async wait<T>(answer: Promise<T>): Promise<T> {
		const budget = this.#budget;
		try {
			if (budget === undefined) return await answer;
			return await withTimeout(answer, Nano.fromMilli(budget), `request timed out after ${budget}ms`);
		} catch (cause) {
			const failure =
				cause instanceof TimeoutError
					? this.#opening > 0
						? new NoStreamCredit({ cause })
						: controlTimeout(cause)
					: error(cause);
			this.#failure = failure;
			// Other failures keep the caller's cancellation ordering.
			if (cause instanceof TimeoutError || cause instanceof NoStreamCredit) {
				for (const owner of this.#owners) owner.abort(failure);
			}
			throw failure;
		}
	}
}
