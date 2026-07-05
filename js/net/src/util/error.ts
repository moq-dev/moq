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

	let code: number | undefined;
	if (err.name === "WebTransportError" && (err as { source?: string }).source === "stream") {
		const c = (err as { streamErrorCode?: number | null }).streamErrorCode;
		code = typeof c === "number" ? c : undefined;
	} else {
		const match = /^(?:RESET_STREAM|STOP_SENDING): (\d+)/.exec(err.message);
		if (!match) return false;
		code = Number(match[1]);
	}

	// It is a stream reset: routine unless the relay signalled a client-actionable fault code.
	return code === undefined || !STREAM_FAULT_CODES.has(code);
}

export function unreachable(value: never): never {
	throw new Error(`unreachable: ${value}`);
}
