import { Milli, Nano } from "@moq/clock";
import { controlTimeout, error, NoStreamCredit } from "../error.ts";
import type { OpenOptions, Stream } from "../stream.ts";
import { TimeoutError, withTimeout } from "./timeout.ts";

/** Request opens defer their deadline to the create-to-answer budget. @internal */
export const requestOptions = { timeout: null, waitUntilAvailable: false } satisfies Partial<OpenOptions>;
/** Milliseconds allowed from a request create to its first answer. @internal */
export const REQUEST_TIMEOUT_MS = 10_000;

/** One fatal deadline for a request, including every stream its setup creates. @internal */
export class Request {
	#opening = false;
	#streams: Stream[] = [];
	#failure?: Error;

	/** Record a create so deadline failures reset its stream even if it opens late. */
	async open(opening: Stream | Promise<Stream>): Promise<Stream> {
		this.#opening = true;
		let stream: Stream;
		try {
			stream = await opening;
		} catch (cause) {
			throw new NoStreamCredit({ cause });
		} finally {
			this.#opening = false;
		}
		if (this.#failure) {
			stream.abort(this.#failure);
			throw this.#failure;
		}
		this.#streams.push(stream);
		return stream;
	}

	/** Wait for the first answer under the request's single deadline. */
	async wait<T>(answer: Promise<T>): Promise<T> {
		try {
			return await withTimeout(
				answer,
				Nano.fromMilli(Milli(REQUEST_TIMEOUT_MS)),
				"request timed out after 10000ms",
			);
		} catch (cause) {
			this.#failure =
				cause instanceof TimeoutError
					? this.#opening
						? new NoStreamCredit({ cause })
						: controlTimeout(cause)
					: error(cause);
			// Other failures keep the caller's cancellation ordering, including legacy UNSUBSCRIBE.
			if (cause instanceof TimeoutError || cause instanceof NoStreamCredit) {
				for (const stream of this.#streams) stream.abort(this.#failure);
			}
			throw this.#failure;
		}
	}
}
