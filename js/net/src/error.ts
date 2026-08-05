/**
 * Errors, including the code a peer reports when it resets a stream or closes the session.
 *
 * @module
 */

/**
 * Codes a peer sends when terminating the session, mirroring the Rust `SessionError`.
 *
 * Specified by moq-lite: 0-31 are moq-transport's codes with moq-transport's meaning,
 * 32-63 are moq-lite's, and 64+ are the application's. {@link StreamCode} is the other
 * registry, and the two are disjoint, so the same integer means different things in each.
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
	/** A required extension was not offered. */
	RequiredExtension: 0x20,
	/** The peer acted against the role it advertised at SETUP. */
	InvalidRole: 0x21,
	/** A stream was opened with an unknown or disallowed type. */
	UnexpectedStream: 0x22,
} as const;

/** A session termination code. See {@link SessionCode}. */
export type SessionCode = (typeof SessionCode)[keyof typeof SessionCode];

/**
 * Codes a peer sends when resetting a stream, mirroring the Rust `StreamError`.
 *
 * The counterpart to {@link SessionCode}, and a disjoint space: a stream reset of 0 is
 * {@link StreamCode.Internal}, not a cancellation ({@link StreamCode.Cancel} is 1).
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
	/** The requested broadcast or track does not exist. */
	NotFound: 0x20,
	/** The broadcast is neither announced nor served, so there is no route to it. */
	Unroutable: 0x21,
	/** The group was superseded by a newer group and dropped. */
	Old: 0x22,
	/** The group was dropped under memory pressure; unlike Old, it can be re-fetched. */
	Evicted: 0x23,
	/** A frame's payload length disagreed with its declared size. */
	WrongSize: 0x24,
	/** A frame declared a payload larger than the receiver accepts. */
	FrameTooLarge: 0x25,
	/** A frame's timestamp doesn't match its track's negotiated timescale. */
	TimestampMismatch: 0x26,
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
 *   if (err instanceof RemoteError && err.code === StreamCode.Old) return;
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
