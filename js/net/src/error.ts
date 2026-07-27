/**
 * Errors, including the code a peer reports when it resets a stream.
 *
 * @module
 */

/**
 * An error the peer reported by resetting a stream, carrying the raw code it sent.
 *
 * The codes are not standardized, so this deliberately does not translate one into a local
 * error: the number means whatever the peer's implementation says it means. A read or write
 * rejects with this on every transport, so branch on {@link code} rather than feature-detecting
 * `WebTransportError`, which a non-browser runtime never defines and the WebSocket fallback
 * never throws.
 *
 * Code 0 is what a transport sends when a stream is dropped or aborted with no code of its own.
 *
 * ```ts
 * try {
 *   frame = await group.readFrame();
 * } catch (err) {
 *   // Whatever this peer's code 2 means to it.
 *   if (err instanceof RemoteError && err.code === 2) return;
 *   throw err;
 * }
 * ```
 *
 * @public
 */
export class RemoteError extends Error {
	/** The code the peer sent, verbatim. */
	readonly code: number;

	constructor(code: number, options?: { cause?: unknown }) {
		super(`remote error: ${code}`, options);
		this.name = "RemoteError";
		this.code = code;
	}
}

/** The WebTransport-shaped fields a stream reset code arrives in. */
type StreamErrorLike = { source?: unknown; streamErrorCode?: unknown };

function streamCode(err: unknown): number | undefined {
	if (typeof err !== "object" || err === null) return undefined;

	const { source, streamErrorCode } = err as StreamErrorLike;
	if (source !== "stream" || typeof streamErrorCode !== "number") return undefined;

	return streamErrorCode;
}

/**
 * Decode a transport failure into a {@link RemoteError} error when it carries a stream reset code,
 * otherwise pass it through.
 *
 * Native WebTransport rejects with a `WebTransportError`; the WebSocket fallback mints an error
 * with the same `source`/`streamErrorCode` fields. Reading the fields rather than the class
 * covers both, and works in a runtime with no `WebTransportError` at all.
 *
 * @internal Called at the transport boundary so the raw error never reaches an application.
 */
export function fromTransport(err: unknown): Error {
	const code = streamCode(err);
	if (code === undefined) return error(err);
	return new RemoteError(code, { cause: err });
}

/**
 * Coerce an unknown thrown value into an `Error`.
 *
 * @internal
 */
export function error(err: unknown): Error {
	return err instanceof Error ? err : new Error(String(err));
}

/**
 * Format an error into a non-empty, human-readable string for logging.
 *
 * Safari always leaves `WebTransportError.message` blank, so a bare `err.message` degrades to
 * an empty string and the reason is lost. This falls back to the error type name and appends
 * the WebTransport `source` and application `streamErrorCode`, so the log line always says
 * something.
 */
export function reason(err: unknown): string {
	const e = error(err);

	// WebTransportError carries the failure origin and the peer's application error code,
	// often the only identifying detail since WebKit leaves `message` empty.
	if (typeof WebTransportError !== "undefined" && e instanceof WebTransportError) {
		const parts = [`source=${e.source}`];
		if (e.streamErrorCode !== null) parts.push(`code=${e.streamErrorCode}`);
		const detail = parts.join(" ");
		return e.message ? `${e.message} (${detail})` : `WebTransportError: ${detail}`;
	}

	return e.message || e.name || "unknown error";
}
