// `ignoreBOM` keeps a leading U+FEFF, which is text like any other, as Rust does.
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Decode a protocol string, rejecting bytes that are not valid UTF-8. */
export function decodeUtf8(buffer: Uint8Array): string {
	return decoder.decode(buffer);
}

/** Throw unless a track name is well-formed: UTF-8 cannot carry a lone surrogate, so the encoder would write U+FFFD, another name's bytes. */
export function checkName(name: string): void {
	if (!name.isWellFormed()) throw new Error("track name contains a lone surrogate");
}
