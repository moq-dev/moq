// I hate javascript.
export function error(err: unknown): Error {
	return err instanceof Error ? err : new Error(String(err));
}

// Stream-reset application codes that indicate a client-actionable FAULT (bad auth, wrong path, protocol
// violation, unroutable, ...) rather than a routine teardown. Mirrors rs/moq-net/src/error.rs
// `Error::to_code()`. Kept as a DENYLIST: any code NOT listed (Cancel=0, Timeout=3, Transport=4,
// Dropped=24, Closed=25, ...) is treated as an expected teardown, so a normal unsubscribe/handover reset
// stays quiet whatever code it carries, while a genuine failure surfaces.
const STREAM_FAULT_CODES = new Set<number>([
	6, // Unauthorized
	9, // Version
	11, // BoundsExceeded
	12, // Duplicate
	13, // NotFound
	14, // WrongSize
	15, // ProtocolViolation
	16, // UnexpectedMessage
	17, // Unsupported
	27, // FrameTooLarge
	30, // Unroutable
]);

/**
 * True when an error is a ROUTINE stream teardown (a peer RESET_STREAM / STOP_SENDING for an unsubscribe,
 * publisher handover, or close) rather than a client-actionable fault. Lets callers log expected lifecycle
 * churn at debug while a genuine failure (auth, not-found, protocol, unroutable, ...) still warns.
 *
 * Reads the application error code the relay encodes (rs/moq-net `error.rs`): `WebTransportError.
 * streamErrorCode` on native transports, or the trailing number of qmux's "RESET_STREAM: <code>" /
 * "STOP_SENDING: <code>" message on the WebSocket fallback. A stream reset whose code is a fault (see
 * STREAM_FAULT_CODES), or an error that is not a stream reset at all, returns false so it is surfaced.
 * Keyed on `err.name` rather than `instanceof WebTransportError` so it is safe where that global is
 * undefined (e.g. the test runner).
 */
export function isStreamAbort(err: unknown): boolean {
	if (!(err instanceof Error)) return false;

	// Coded stream resets decide first, so a client-actionable fault code always wins over the message
	// heuristics below. It is a stream reset: routine unless the relay signalled a fault code.
	if (err.name === "WebTransportError") {
		// Trust the relay's numeric app code regardless of `source`: Chrome reports a WRITE-side abort
		// (a downstream unsubscribe seen by the publisher) as source "session", not "stream", so gating on
		// source would leave that routine teardown warning on every closed viewer.
		const c = (err as { streamErrorCode?: number | null }).streamErrorCode;
		const code = typeof c === "number" ? c : undefined;
		return code === undefined || !STREAM_FAULT_CODES.has(code);
	}

	const match = /^(?:RESET_STREAM|STOP_SENDING): (\d+)/.exec(err.message);
	if (match) return !STREAM_FAULT_CODES.has(Number(match[1]));

	// The session itself ended, which fails every stream still open on it. qmux words this
	// "Connection closed", optionally carrying the peer's CONNECTION_CLOSE code. Same rule as a stream
	// reset: routine unless the peer signalled a client-actionable fault. (Native WebTransport reports
	// the equivalent as a WebTransportError with no `streamErrorCode`, handled above.)
	const closed = /^Connection closed(?::\s*(\d+))?/.exec(err.message);
	if (closed) {
		const code = closed[1] === undefined ? undefined : Number(closed[1]);
		return code === undefined || !STREAM_FAULT_CODES.has(code);
	}

	// A write/close after the stream already ended surfaces as a generic Streams-API error rather than a
	// coded RESET_STREAM/STOP_SENDING. This is routine teardown (a peer reset racing an in-flight write),
	// common over the WebSocket/qmux fallback. Engines word it differently: Chromium/Firefox say the stream
	// is "closed or closing"; Safari throws InvalidStateError. Key Safari on `name` rather than the prose,
	// so an unrelated error that merely mentions an invalid state still surfaces.
	if (err.name === "InvalidStateError") return true;
	return /closed or closing/i.test(err.message);
}

export function unreachable(value: never): never {
	throw new Error(`unreachable: ${value}`);
}
