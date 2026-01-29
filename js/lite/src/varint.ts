// QUIC variable-length integer encoding/decoding
// https://www.rfc-editor.org/rfc/rfc9000#section-16

export const MAX_U6 = 2 ** 6 - 1;
export const MAX_U14 = 2 ** 14 - 1;
export const MAX_U30 = 2 ** 30 - 1;
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

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
