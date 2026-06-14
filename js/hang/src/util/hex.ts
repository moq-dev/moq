/** Decode a hex string (with optional `0x` prefix) into bytes. */
export function toBytes(hex: string) {
	hex = hex.startsWith("0x") ? hex.slice(2) : hex;
	if (hex.length % 2) {
		throw new Error("invalid hex string length");
	}

	const matches = hex.match(/.{2}/g);
	if (!matches) {
		throw new Error("invalid hex string format");
	}

	return new Uint8Array(matches.map((byte) => parseInt(byte, 16)));
}

/** Encode bytes as a lowercase hex string. */
export function fromBytes(bytes: Uint8Array) {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
