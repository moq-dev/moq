// QUIC variable-length integer encoding/decoding
// https://www.rfc-editor.org/rfc/rfc9000#section-16

export const MAX_U6 = 2 ** 6 - 1;
export const MAX_U14 = 2 ** 14 - 1;
export const MAX_U30 = 2 ** 30 - 1;
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

// Leading-ones varint encoding/decoding (draft-17 Section 1.4.1)
// Encoding scheme:
// 0xxxxxxx        → 1 byte  (7 bits)
// 10xxxxxx + 1B   → 2 bytes (14 bits)
// 110xxxxx + 2B   → 3 bytes (21 bits)
// 1110xxxx + 3B   → 4 bytes (28 bits)
// 11110xxx + 4B   → 5 bytes (35 bits)
// 111110xx + 5B   → 6 bytes (42 bits)
// 1111110x        → INVALID
// 11111110 + 7B   → 8 bytes (56 bits)
// 11111111 + 8B   → 9 bytes (64 bits)

const MAX_U64 = (1n << 64n) - 1n;

export function sizeLeadingOnes(v: number | bigint): number {
	const b = BigInt(v);
	if (b < 0n) throw new RangeError(`value is negative: ${v}`);
	if (b > MAX_U64) throw new RangeError(`value exceeds 64 bits: ${v}`);
	if (b < 1n << 7n) return 1;
	if (b < 1n << 14n) return 2;
	if (b < 1n << 21n) return 3;
	if (b < 1n << 28n) return 4;
	if (b < 1n << 35n) return 5;
	if (b < 1n << 42n) return 6;
	if (b < 1n << 56n) return 8;
	return 9;
}

export function encodeLeadingOnesTo(dst: ArrayBuffer, v: number | bigint): Uint8Array {
	const x = BigInt(v);
	if (x < 0n) throw new RangeError(`underflow, value is negative: ${v}`);
	if (x > MAX_U64) throw new RangeError(`value exceeds 64 bits: ${v}`);

	const view = new DataView(dst);

	if (x < 1n << 7n) {
		view.setUint8(0, Number(x));
		return new Uint8Array(dst, 0, 1);
	}
	if (x < 1n << 14n) {
		view.setUint8(0, 0x80 | Number(x >> 8n));
		view.setUint8(1, Number(x & 0xffn));
		return new Uint8Array(dst, 0, 2);
	}
	if (x < 1n << 21n) {
		view.setUint8(0, 0xc0 | Number(x >> 16n));
		view.setUint16(1, Number(x & 0xffffn));
		return new Uint8Array(dst, 0, 3);
	}
	if (x < 1n << 28n) {
		view.setUint8(0, 0xe0 | Number(x >> 24n));
		view.setUint8(1, Number((x >> 16n) & 0xffn));
		view.setUint16(2, Number(x & 0xffffn));
		return new Uint8Array(dst, 0, 4);
	}
	if (x < 1n << 35n) {
		view.setUint8(0, 0xf0 | Number(x >> 32n));
		view.setUint32(1, Number(x & 0xffffffffn));
		return new Uint8Array(dst, 0, 5);
	}
	if (x < 1n << 42n) {
		view.setUint8(0, 0xf8 | Number(x >> 40n));
		view.setUint8(1, Number((x >> 32n) & 0xffn));
		view.setUint32(2, Number(x & 0xffffffffn));
		return new Uint8Array(dst, 0, 6);
	}
	if (x < 1n << 56n) {
		// 11111110 + 7 bytes
		view.setUint8(0, 0xfe);
		view.setUint8(1, Number((x >> 48n) & 0xffn));
		view.setUint16(2, Number((x >> 32n) & 0xffffn));
		view.setUint32(4, Number(x & 0xffffffffn));
		return new Uint8Array(dst, 0, 8);
	}
	// 11111111 + 8 bytes
	view.setUint8(0, 0xff);
	view.setBigUint64(1, x);
	return new Uint8Array(dst, 0, 9);
}

export function encodeLeadingOnes(v: number | bigint): Uint8Array {
	return encodeLeadingOnesTo(new ArrayBuffer(9), v);
}

export function decodeLeadingOnes(buf: Uint8Array): [bigint, Uint8Array] {
	if (buf.length === 0) throw new Error("buffer is empty");

	const b = buf[0];
	// Count leading 1-bits
	let ones = 0;
	for (let bit = 7; bit >= 0; bit--) {
		if (b & (1 << bit)) ones++;
		else break;
	}

	if (ones === 6) throw new Error("invalid leading-ones varint: 1111110x prefix is reserved");

	let totalSize: number;
	if (ones <= 5) totalSize = ones + 1;
	else if (ones === 7) totalSize = 8;
	else totalSize = 9; // ones === 8

	if (buf.length < totalSize) {
		throw new Error(`buffer too short: need ${totalSize} bytes, have ${buf.length}`);
	}

	const view = new DataView(buf.buffer, buf.byteOffset, totalSize);
	const remain = buf.subarray(totalSize);
	let value: bigint;

	switch (ones) {
		case 0:
			value = BigInt(b);
			break;
		case 1:
			value = (BigInt(b & 0x3f) << 8n) | BigInt(buf[1]);
			break;
		case 2:
			value = (BigInt(b & 0x1f) << 16n) | BigInt(view.getUint16(1));
			break;
		case 3:
			value = (BigInt(b & 0x0f) << 24n) | (BigInt(buf[1]) << 16n) | (BigInt(buf[2]) << 8n) | BigInt(buf[3]);
			break;
		case 4:
			value = (BigInt(b & 0x07) << 32n) | BigInt(view.getUint32(1));
			break;
		case 5:
			value =
				(BigInt(b & 0x03) << 40n) |
				(BigInt(buf[1]) << 32n) |
				(BigInt(buf[2]) << 24n) |
				(BigInt(buf[3]) << 16n) |
				(BigInt(buf[4]) << 8n) |
				BigInt(buf[5]);
			break;
		case 7: {
			// 11111110 + 7 bytes = 56 usable bits
			const hi = new Uint8Array(8);
			hi[0] = 0;
			hi.set(buf.subarray(1, 8), 1);
			value = new DataView(hi.buffer).getBigUint64(0);
			break;
		}
		case 8: {
			// 11111111 + 8 bytes = 64 bits
			value = new DataView(buf.buffer, buf.byteOffset + 1, 8).getBigUint64(0);
			break;
		}
		default:
			throw new Error("impossible");
	}

	return [value, remain];
}

/**
 * Returns the number of bytes needed to encode a value as a varint.
 */
export function size(v: number): number {
	if (v <= MAX_U6) return 1;
	if (v <= MAX_U14) return 2;
	if (v <= MAX_U30) return 4;
	if (v <= MAX_U53) return 8;
	throw new Error(`overflow, value larger than 53-bits: ${v}`);
}

// Helper functions for writing to an ArrayBuffer
function setUint8(dst: ArrayBuffer, v: number): Uint8Array {
	const buffer = new Uint8Array(dst, 0, 1);
	buffer[0] = v;
	return buffer;
}

function setUint16(dst: ArrayBuffer, v: number): Uint8Array {
	const view = new DataView(dst, 0, 2);
	view.setUint16(0, v);
	return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

function setUint32(dst: ArrayBuffer, v: number): Uint8Array {
	const view = new DataView(dst, 0, 4);
	view.setUint32(0, v);
	return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

function setUint64(dst: ArrayBuffer, v: bigint): Uint8Array {
	const view = new DataView(dst, 0, 8);
	view.setBigUint64(0, v);
	return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

const MAX_U62 = 2n ** 62n - 1n;

/**
 * Encodes a number or bigint into a scratch buffer.
 * Used by stream.ts to avoid allocations.
 */
export function encodeTo(dst: ArrayBuffer, v: number | bigint): Uint8Array {
	const b = BigInt(v);
	if (b < 0n) {
		throw new Error(`underflow, value is negative: ${v}`);
	}
	if (b > MAX_U62) {
		throw new Error(`overflow, value larger than 62-bits: ${v}`);
	}
	const n = Number(b);
	if (n <= MAX_U6) {
		return setUint8(dst, n);
	}
	if (n <= MAX_U14) {
		return setUint16(dst, n | 0x4000);
	}
	if (n <= MAX_U30) {
		return setUint32(dst, n | 0x80000000);
	}
	return setUint64(dst, b | 0xc000000000000000n);
}

/**
 * Encodes a number as a QUIC variable-length integer.
 * Returns a new Uint8Array containing the encoded bytes.
 */
export function encode(v: number): Uint8Array {
	return encodeTo(new ArrayBuffer(8), v);
}

/**
 * Decodes a QUIC variable-length integer from a buffer.
 * Returns a tuple of [value, remaining buffer].
 */
export function decode(buf: Uint8Array): [number, Uint8Array] {
	if (buf.length === 0) {
		throw new Error("buffer is empty");
	}

	const size = 1 << ((buf[0] & 0xc0) >> 6);

	if (buf.length < size) {
		throw new Error(`buffer too short: need ${size} bytes, have ${buf.length}`);
	}

	const view = new DataView(buf.buffer, buf.byteOffset, size);
	const remain = buf.subarray(size);

	let v: number;

	if (size === 1) {
		v = buf[0] & 0x3f;
	} else if (size === 2) {
		v = view.getUint16(0) & 0x3fff;
	} else if (size === 4) {
		v = view.getUint32(0) & 0x3fffffff;
	} else if (size === 8) {
		// NOTE: Precision loss above 2^53, but we're using number type
		v = Number(view.getBigUint64(0) & 0x3fffffffffffffffn);
	} else {
		throw new Error("impossible");
	}

	return [v, remain];
}
