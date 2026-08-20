/**
 * Errors, including the code a peer reports when it resets a stream or closes the session.
 *
 * @module
 */

/** The nominal brand for session termination codes. */
declare const SESSION_CODE: unique symbol;

/** A code from the session termination registry. */
export type SessionCode = number & { readonly [SESSION_CODE]: true };

/**
 * Codes a peer sends when terminating the session, mirroring the Rust `SessionError`.
 *
 * Specified by moq-lite, which reuses moq-transport's codes unchanged. Call `SessionCode(code)`
 * to construct an application code in the 64+ range. {@link StreamCode} is the other registry,
 * and the two are disjoint, so the same integer means different things in each.
 *
 * Codes 32-63 are reserved rather than assigned. An implementation may send one for a
 * condition with no code here, but the draft gives it no meaning, so treat anything not
 * listed below as an unspecified error rather than guessing.
 *
 * @public
 */
export const SessionCode = Object.freeze(
	Object.assign((code: number): SessionCode => applicationCode(code) as SessionCode, {
		/** Ending the session normally, with no error. */
		Cancel: 0x0 as SessionCode,
		/** Something went wrong that isn't worth a dedicated code. */
		Internal: 0x1 as SessionCode,
		/** The credentials don't grant the requested path or operation. Retrying will fail again. */
		Unauthorized: 0x2 as SessionCode,
		/** A protocol rule was broken; the session is unusable. */
		ProtocolViolation: 0x3 as SessionCode,
		/** A key-value pair was malformed or repeated more than allowed. */
		KeyValueFormatting: 0x6 as SessionCode,
		/** The peer did not close within the GOAWAY drain deadline. */
		GoawayTimeout: 0x10 as SessionCode,
		/** A control message took too long. */
		Timeout: 0x11 as SessionCode,
		/** No version could be negotiated. */
		Version: 0x15 as SessionCode,
	} as const),
);

/** The nominal brand for stream reset codes. */
declare const STREAM_CODE: unique symbol;

/** A code from the stream reset registry. */
export type StreamCode = number & { readonly [STREAM_CODE]: true };

/**
 * Codes a peer sends when resetting a stream, mirroring the Rust `StreamError`.
 *
 * The counterpart to {@link SessionCode}, and a disjoint space: a stream reset of 0 is
 * {@link StreamCode.Internal}, not a cancellation ({@link StreamCode.Cancel} is 1). Call
 * `StreamCode(code)` to construct an application code in the 64+ range.
 *
 * Codes 32-63 are reserved rather than assigned, same as {@link SessionCode}.
 *
 * @public
 */
export const StreamCode = Object.freeze(
	Object.assign((code: number): StreamCode => applicationCode(code) as StreamCode, {
		/** Something went wrong that isn't worth a dedicated code. */
		Internal: 0x0 as StreamCode,
		/** The sender is done with this stream, not failing. A routine unsubscribe. */
		Cancel: 0x1 as StreamCode,
		/** The content missed its delivery deadline. */
		DeliveryTimeout: 0x2 as StreamCode,
		/** The session ended, taking this stream with it. */
		SessionClosed: 0x3 as StreamCode,
		/** The session is going away (a GOAWAY was received). */
		GoingAway: 0x4 as StreamCode,
		/** The reader fell too far behind and content was dropped to catch up. */
		TooFarBehind: 0x5 as StreamCode,
		/** The track's content could not be parsed. */
		MalformedTrack: 0x12 as StreamCode,
	} as const),
);

function applicationCode(code: number): number {
	if (!Number.isInteger(code) || code < 64 || code > 0xffffffff) {
		throw new RangeError(`invalid application error code: ${code}`);
	}
	return code;
}

/**
 * An error the peer reported by closing the session, carrying its {@link SessionCode}.
 *
 * This surfaces on every transport, so catch this type rather than feature-detecting
 * `WebTransportError`, which a non-browser runtime never defines and the WebSocket fallback
 * never throws.
 *
 * ```ts
 * const err = await connection.closed;
 * if (err instanceof SessionError && err.code === SessionCode.Unauthorized) {
 *   console.warn("server rejected the session");
 * }
 * ```
 *
 * @public
 */
export class SessionError extends Error {
	/** The session code the peer sent, verbatim. */
	readonly code: SessionCode;

	constructor(code: SessionCode, options?: { cause?: unknown; reason?: string }) {
		super(options?.reason ? `remote error: ${code} (${options.reason})` : `remote error: ${code}`, options);
		this.name = "SessionError";
		this.code = code;
	}
}

/**
 * An error the peer reported by resetting a stream, carrying its {@link StreamCode}.
 *
 * This surfaces on every transport, so catch this type rather than feature-detecting
 * `WebTransportError`, which a non-browser runtime never defines and the WebSocket fallback
 * never throws.
 *
 * ```ts
 * try {
 *   frame = await group.readFrame();
 * } catch (err) {
 *   if (err instanceof StreamError && err.code === StreamCode.Cancel) return;
 *   throw err;
 * }
 * ```
 *
 * @public
 */
export class StreamError extends Error {
	/** The stream code the peer sent, verbatim. */
	readonly code: StreamCode;

	constructor(code: StreamCode, options?: { cause?: unknown; reason?: string }) {
		super(options?.reason ? `remote error: ${code} (${options.reason})` : `remote error: ${code}`, options);
		this.name = "StreamError";
		this.code = code;
	}
}

/** The WebTransport-shaped fields a stream reset code arrives in. */
type StreamErrorLike = { source?: unknown; streamErrorCode?: unknown };

function streamCode(err: unknown): StreamCode | undefined {
	if (typeof err !== "object" || err === null) return undefined;

	const { source, streamErrorCode } = err as StreamErrorLike;
	if (source !== "stream" || typeof streamErrorCode !== "number") return undefined;

	return streamErrorCode as StreamCode;
}

/**
 * Decode a transport failure into a {@link StreamError} when it carries a stream reset code,
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
	return new StreamError(code, { cause: err });
}

const legacyWebTransportErrors = new WeakSet<object>();

/**
 * Build the `reason` to hand `abort()` / `cancel()` so the transport puts `code` on the wire.
 *
 * Both the WebTransport spec and the WebSocket fallback take the reset code from a
 * `WebTransportError`'s `streamErrorCode` and send 0 for anything else, a plain `Error`
 * included. Since 0 is {@link StreamCode.Internal}, cancelling with a bare `Error` tells the
 * peer we failed rather than that we are done.
 *
 * The native constructor exists exactly where native WebTransport does, which is where the
 * fallback isn't used, so mint a matching shape elsewhere rather than feature-detect a global
 * that will not be there.
 *
 * @internal
 */
export function toTransport(code: StreamCode, message: string): Error {
	const Native = (globalThis as { WebTransportError?: typeof WebTransportError }).WebTransportError;
	if (Native) {
		const Legacy = Native as unknown as new (init: { message: string; streamErrorCode: number }) => Error;
		if (legacyWebTransportErrors.has(Native)) return new Legacy({ message, streamErrorCode: code });

		try {
			return new Native(message, { source: "stream", streamErrorCode: code });
		} catch (err) {
			// Chromium still implements the previous single-dictionary constructor.
			if (!(err instanceof TypeError)) throw err;
			legacyWebTransportErrors.add(Native);
			return new Legacy({ message, streamErrorCode: code });
		}
	}

	return Object.assign(new Error(message), { source: "stream" as const, streamErrorCode: code });
}

/**
 * Decode a session close into its terminal error: `null` for a clean close
 * ({@link SessionCode.Cancel}), otherwise a {@link SessionError} carrying the peer's code.
 *
 * @internal Applied to the transport's `closed` info so the code survives to the application.
 */
export function fromClose(info: WebTransportCloseInfo): SessionError | null {
	const code = (info.closeCode ?? SessionCode.Cancel) as SessionCode;
	if (code === SessionCode.Cancel) return null;
	return new SessionError(code, { reason: info.reason });
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
