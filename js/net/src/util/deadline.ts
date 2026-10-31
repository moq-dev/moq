import { Milli, Nano } from "@moq/clock";
import { controlTimeout, error, NoStreamCredit } from "../error.ts";
import type { OpenOptions, Stream } from "../stream.ts";
import { TimeoutError, withTimeout } from "./timeout.ts";

/** Request opens defer their deadline to the create-to-answer budget. @internal */
export const requestOptions = { timeout: null, waitUntilAvailable: false } satisfies Partial<OpenOptions>;
/** Milliseconds allowed from a request create to its first answer. @internal */
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

/** One fatal deadline for a request, including every stream its setup creates. @internal */
export class Deadline {
	#opening = false;
	#owners: Abortable[] = [];
	#failure?: Error;

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
		this.#opening = true;
		let stream: Stream;
		try {
			stream = await opening;
		} catch (cause) {
			if (sessionFailure(cause)) throw cause;
			throw new NoStreamCredit({ cause });
		} finally {
			this.#opening = false;
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
		try {
			return await withTimeout(
				answer,
				Nano.fromMilli(Milli(REQUEST_TIMEOUT_MS)),
				`request timed out after ${REQUEST_TIMEOUT_MS}ms`,
			);
		} catch (cause) {
			const failure =
				cause instanceof TimeoutError
					? this.#opening
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
