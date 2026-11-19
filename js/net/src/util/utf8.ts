// `ignoreBOM` keeps a leading U+FEFF, which is text like any other, as Rust does.
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Decode a protocol string, rejecting bytes that are not valid UTF-8. */
export function decodeUtf8(buffer: Uint8Array): string {
	return decoder.decode(buffer);
}
