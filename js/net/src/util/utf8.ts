const decoder = new TextDecoder("utf-8", { fatal: true });

/** Decode a protocol string, rejecting bytes that are not valid UTF-8. */
export function decodeUtf8(buffer: Uint8Array): string {
	return decoder.decode(buffer);
}
