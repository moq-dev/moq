// z-base-32: most significant bit first, no padding, trailing bits must be zero.
const ALPHABET = "ybndrfg8ejkmcpqxot1uwisza345h769";

/** Encode `bytes` as z-base-32. */
export function encode(bytes: Uint8Array): string {
	let out = "";
	let buffer = 0;
	let bits = 0;
	for (const byte of bytes) {
		buffer = (buffer << 8) | byte;
		bits += 8;
		while (bits >= 5) {
			bits -= 5;
			out += ALPHABET[(buffer >> bits) & 31];
		}
		buffer &= (1 << bits) - 1;
	}
	if (bits > 0) out += ALPHABET[(buffer << (5 - bits)) & 31];
	return out;
}

/**
 * Decode exactly `length` bytes, or `undefined` for any other text. The alphabet is
 * lowercase-only and nonzero trailing bits are refused, so a value has one text form.
 */
export function decode(text: string, length: number): Uint8Array<ArrayBuffer> | undefined {
	if (text.length !== Math.ceil((length * 8) / 5)) return undefined;
	const out = new Uint8Array(length);
	let buffer = 0;
	let bits = 0;
	let offset = 0;
	for (const char of text) {
		const value = ALPHABET.indexOf(char);
		if (value < 0) return undefined;
		buffer = (buffer << 5) | value;
		bits += 5;
		if (bits >= 8) {
			bits -= 8;
			out[offset++] = (buffer >> bits) & 0xff;
		}
		buffer &= (1 << bits) - 1;
	}
	return buffer === 0 ? out : undefined;
}
