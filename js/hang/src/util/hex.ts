const HEX = /^[0-9a-fA-F]*$/;

/**
 * Decode a hex string (with optional `0x` prefix) into bytes.
 *
 * Throws on anything that isn't hex. Catalog binary fields (`description`) are hex while the CMAF
 * `init` segment is base64, so a base64 value reaches this often enough to be worth rejecting
 * loudly: `parseInt` stops at the first bad character rather than failing, so `"AU"` would decode
 * to `0x0a` and hand the decoder a plausible-looking buffer of garbage.
 */
export function toBytes(hex: string) {
	hex = hex.startsWith("0x") ? hex.slice(2) : hex;
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

/** Encode bytes as a lowercase hex string. */
export function fromBytes(bytes: Uint8Array) {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
