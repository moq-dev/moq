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

/** One fatal deadline for a request, including every stream its setup creates. @internal */
export class Deadline {
	/** Creates still waiting on credit: a fetch opens its TRACK stream alongside. */
	#opening = 0;
	#streams: Stream[] = [];
	#failure?: Error;

	/** Record a create so deadline failures reset its stream even if it opens late. */
	async open(opening: Stream | Promise<Stream>): Promise<Stream> {
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
		if (this.#failure) {
			stream.abort(this.#failure);
			throw this.#failure;
		}
		this.#streams.push(stream);
		return stream;
	}

	/** Write the request on an opened stream, which a deadline failure after resets. */
	async send(stream: Stream, write: () => Promise<void>): Promise<void> {
		if (this.#failure) throw this.#failure;
		if (!this.#streams.includes(stream)) throw new Error("request sent on a stream the deadline did not open");
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
					? this.#opening > 0
						? new NoStreamCredit({ cause })
						: controlTimeout(cause)
					: error(cause);
			this.#failure = failure;
			// Other failures keep the caller's cancellation ordering.
			if (cause instanceof TimeoutError || cause instanceof NoStreamCredit) {
				for (const stream of this.#streams) stream.abort(failure);
			}
			throw failure;
		}
	}
}
