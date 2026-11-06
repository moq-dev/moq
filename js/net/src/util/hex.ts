const HEX = /^[0-9a-fA-F]*$/;

/**
 * Decode a hex string (with optional `0x` prefix) into bytes.
 *
 * Throws on anything that isn't hex: `parseInt` stops at the first bad character rather than
 * failing, so a typo in a pinned certificate hash would otherwise pin a different hash.
 */
export function toBytes(hex: string): Uint8Array<ArrayBuffer> {
	hex = hex.startsWith("0x") ? hex.slice(2) : hex;
	// A zero-length certificate hash can never match, so an empty value is a config error.
	if (!hex) {
		throw new Error("empty hex string");
	}
	if (hex.length % 2) {
		throw new Error("invalid hex string length");
	}
	if (!HEX.test(hex)) {
		const index = hex.search(/[^0-9a-fA-F]/);
		throw new Error(`invalid hex character '${hex[index]}' at position ${index}`);
	}

	const bytes = new Uint8Array(hex.length / 2);
	for (let i = 0; i < bytes.length; i++) {
		bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
	}

	return bytes;
}

export function fromBytes(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
