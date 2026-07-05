// I hate javascript.
export function error(err: unknown): Error {
	return err instanceof Error ? err : new Error(String(err));
}

/**
 * True when an error is a normal stream teardown (peer RESET_STREAM/STOP_SENDING, downstream unsubscribe,
 * publisher handover) rather than a protocol or application failure. Lets callers log expected lifecycle
 * errors at debug instead of warn. Covers WebTransport (typed source; the message wording varies by
 * browser and is often empty on Safari) and the WebSocket/qmux fallback (stable RESET_STREAM/STOP_SENDING
 * messages). Keyed on `err.name` rather than `instanceof WebTransportError` so it is safe where that
 * global is undefined (e.g. the test runner).
 */
export function isStreamAbort(err: unknown): boolean {
	if (!(err instanceof Error)) return false;
	if (err.name === "WebTransportError" && (err as { source?: string }).source === "stream") return true;
	return /^(RESET_STREAM|STOP_SENDING)/.test(err.message);
}

export function unreachable(value: never): never {
	throw new Error(`unreachable: ${value}`);
}
