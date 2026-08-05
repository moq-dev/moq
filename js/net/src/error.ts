/**
 * Errors, including the code a peer reports when it resets a stream or closes the session.
 *
 * @module
 */

/**
 * Codes a peer sends when terminating the session, mirroring the Rust `SessionError`.
 *
 * Specified by moq-lite, which reuses moq-transport's codes unchanged; 64 and up are the
 * application's. {@link StreamCode} is the other registry, and the two are disjoint, so the
 * same integer means different things in each.
 *
 * Codes 32-63 are reserved rather than assigned. An implementation may send one for a
 * condition with no code here, but the draft gives it no meaning, so treat anything not
 * listed below as an unspecified error rather than guessing.
 *
 * @public
 */
export const SessionCode = {
	/** Ending the session normally, with no error. */
	Cancel: 0x0,
	/** Something went wrong that isn't worth a dedicated code. */
	Internal: 0x1,
	/** The credentials don't grant the requested path or operation. Retrying will fail again. */
	Unauthorized: 0x2,
	/** A protocol rule was broken; the session is unusable. */
	ProtocolViolation: 0x3,
	/** A key-value pair was malformed or repeated more than allowed. */
	KeyValueFormatting: 0x6,
	/** The peer did not close within the GOAWAY drain deadline. */
	GoawayTimeout: 0x10,
	/** A control message took too long. */
	Timeout: 0x11,
	/** No version could be negotiated. */
	Version: 0x15,
} as const;

/** A session termination code. See {@link SessionCode}. */
export type SessionCode = (typeof SessionCode)[keyof typeof SessionCode];

/**
 * Codes a peer sends when resetting a stream, mirroring the Rust `StreamError`.
 *
 * The counterpart to {@link SessionCode}, and a disjoint space: a stream reset of 0 is
 * {@link StreamCode.Internal}, not a cancellation ({@link StreamCode.Cancel} is 1).
 *
 * Codes 32-63 are reserved rather than assigned, same as {@link SessionCode}.
 *
 * @public
 */
export const StreamCode = {
	/** Something went wrong that isn't worth a dedicated code. */
	Internal: 0x0,
	/** The sender is done with this stream, not failing. A routine unsubscribe. */
	Cancel: 0x1,
	/** The content missed its delivery deadline. */
	DeliveryTimeout: 0x2,
	/** The session ended, taking this stream with it. */
	SessionClosed: 0x3,
	/** The session is going away (a GOAWAY was received). */
	GoingAway: 0x4,
	/** The reader fell too far behind and content was dropped to catch up. */
	TooFarBehind: 0x5,
	/** The track's content could not be parsed. */
	MalformedTrack: 0x12,
} as const;

/** A stream reset code. See {@link StreamCode}. */
export type StreamCode = (typeof StreamCode)[keyof typeof StreamCode];

/**
 * An error the peer reported by resetting a stream or closing the session, carrying the
 * code it sent.
 *
 * Which registry {@link code} belongs to depends on what failed: a stream read or write
 * rejects with a {@link StreamCode}, while a session close carries a {@link SessionCode}.
 * The two spaces are disjoint, so read it against the right one. This surfaces on every
 * transport, so branch on {@link code} rather than feature-detecting `WebTransportError`,
 * which a non-browser runtime never defines and the WebSocket fallback never throws.
 *
 * ```ts
 * try {
 *   frame = await group.readFrame();
 * } catch (err) {
 *   if (err instanceof RemoteError && err.code === StreamCode.Cancel) return;
 *   throw err;
 * }
 * ```
 *
 * @public
 */
export class RemoteError extends Error {
	/** The code the peer sent, verbatim. */
	readonly code: number;

	constructor(code: number, options?: { cause?: unknown; reason?: string }) {
		super(options?.reason ? `remote error: ${code} (${options.reason})` : `remote error: ${code}`, options);
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
export function toTransport(code: number, message: string): Error {
	const Native = (globalThis as { WebTransportError?: typeof WebTransportError }).WebTransportError;
	if (Native) return new Native(message, { source: "stream", streamErrorCode: code });

	return Object.assign(new Error(message), { source: "stream" as const, streamErrorCode: code });
}

/**
 * Decode a session close into its terminal error: `null` for a clean close
 * ({@link SessionCode.Cancel}), otherwise a {@link RemoteError} carrying the peer's code.
 *
 * @internal Applied to the transport's `closed` info so the code survives to the application.
 */
export function fromClose(info: WebTransportCloseInfo): RemoteError | null {
	const code = info.closeCode ?? SessionCode.Cancel;
	if (code === SessionCode.Cancel) return null;
	return new RemoteError(code, { reason: info.reason });
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
