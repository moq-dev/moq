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

// A stream the deadline resets, with the request written on it and the message withdrawing it.
interface Recorded {
	stream: Stream;
	written?: Promise<boolean>;
	withdraw?: () => Promise<void>;
}

/** One fatal deadline for a request, including every stream its setup creates. @internal */
export class Deadline {
	#opening = false;
	#streams: Recorded[] = [];
	#failure?: Error;

	/** Record a create so deadline failures reset its stream even if it opens late. */
	async open(opening: Stream | Promise<Stream>): Promise<Stream> {
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
		if (this.#failure) {
			stream.abort(this.#failure);
			throw this.#failure;
		}
		this.#streams.push({ stream });
		return stream;
	}

	/**
	 * Write the request on an opened stream, and what withdraws it if the deadline fails after.
	 *
	 * A draft-14-16 request rides the control stream, where a reset tells the peer nothing, so
	 * one that reached the peer is withdrawn by message first. Without `withdraw` the reset alone ends it.
	 */
	async send(stream: Stream, write: () => Promise<void>, withdraw?: () => Promise<void>): Promise<void> {
		if (this.#failure) throw this.#failure;
		const recorded = this.#streams.find((recorded) => recorded.stream === stream);
		if (!recorded) throw new Error("request sent on a stream the deadline did not open");
		const sending = write();
		recorded.written = sending.then(
			() => true,
			() => false,
		);
		recorded.withdraw = withdraw;
		await sending;
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
			// Other failures keep the caller's cancellation ordering, including legacy UNSUBSCRIBE.
			if (cause instanceof TimeoutError || cause instanceof NoStreamCredit) {
				await Promise.all(this.#streams.map((recorded) => Deadline.#reset(recorded, failure)));
			}
			throw failure;
		}
	}

	static async #reset({ stream, written, withdraw }: Recorded, failure: Error) {
		// The withdrawal names a request the peer must already have, so a write still in
		// flight finishes first, and one that never landed is not withdrawn.
		if (withdraw && written && (await written)) await withdraw().catch(() => undefined);
		stream.abort(failure);
	}
}
