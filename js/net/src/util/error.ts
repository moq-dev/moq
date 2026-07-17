// I hate javascript.
export function error(err: unknown): Error {
	return err instanceof Error ? err : new Error(String(err));
}

/**
 * Format an error into a non-empty, human-readable string for logging.
 *
 * Safari always leaves `WebTransportError.message` blank, so a bare `err.message`
 * degrades to an empty string and the reason is lost. This falls back to the error
 * type name and appends the WebTransport `source` and application `streamErrorCode`
 * (the code that identifies a RESET_STREAM), so the log line always says something.
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

export function unreachable(value: never): never {
	throw new Error(`unreachable: ${value}`);
}
