// The varint wire codec on 32-bit halves (see ./u64.ts), so neither format needs a BigInt.
// Package-internal: the public API in ../varint.ts and the stream Cursor and Writer are built on it.
//
// QUIC (RFC 9000 Section 16): the top two bits give the size.
// 00xxxxxx → 1 byte, 01 → 2, 10 → 4, 11 → 8 (62 bits)
//
// Leading-ones (moq-transport draft-17+ Section 1.4.1): the leading 1-bits give the size.
// 0xxxxxxx        → 1 byte  (7 bits)
// 10xxxxxx + 1B   → 2 bytes (14 bits)
// 110xxxxx + 2B   → 3 bytes (21 bits)
// 1110xxxx + 3B   → 4 bytes (28 bits)
// 11110xxx + 4B   → 5 bytes (35 bits)
// 111110xx + 5B   → 6 bytes (42 bits)
// 1111110x + 6B   → 7 bytes (49 bits), draft-18+ only (invalid in draft-17 per #1595)
// 11111110 + 7B   → 8 bytes (56 bits)
// 11111111 + 8B   → 9 bytes (64 bits)

import { POW32, toBigInt } from "./u64.ts";

/**
 * The upper half of the last value decoded or split. Each returns its lower half and leaves the
 * upper one here rather than allocating a pair; read it before the next call.
 */
export const parts = { hi: 0 };

/** Split a non-negative integer of up to 64 bits, returning the lower half. The encoder bounds it. */
export function split(v: number | bigint): number {
	if (typeof v === "number") {
		if (!Number.isInteger(v)) throw new RangeError(`not an integer: ${v}`);
		if (v < 0) throw new RangeError(`underflow, value is negative: ${v}`);
		if (v >= POW32 * POW32) throw new RangeError(`value exceeds 64 bits: ${v}`);
		parts.hi = Math.floor(v / POW32);
		return v >>> 0;
	}
	if (v < 0n) throw new RangeError(`underflow, value is negative: ${v}`);
	if (v >> 64n) throw new RangeError(`value exceeds 64 bits: ${v}`);
	parts.hi = Number(v >> 32n);
	return Number(v & 0xffffffffn);
}

function u32(buf: Uint8Array, o: number): number {
	return ((buf[o] << 24) | (buf[o + 1] << 16) | (buf[o + 2] << 8) | buf[o + 3]) >>> 0;
}

function setU32(buf: Uint8Array, o: number, v: number) {
	buf[o] = v >>> 24;
	buf[o + 1] = v >>> 16;
	buf[o + 2] = v >>> 8;
	buf[o + 3] = v;
}

/** The size of a QUIC varint, from its first byte. */
export function peekQuic(first: number): number {
	return 1 << (first >> 6);
}

/** Decode a QUIC varint of `size` bytes at `o`, returning the lower half and setting `parts.hi`. The bytes must be there. */
export function readQuic(buf: Uint8Array, o: number, size: number): number {
	const b = buf[o] & 0x3f;
	if (size === 8) {
		parts.hi = (b << 24) | (buf[o + 1] << 16) | (buf[o + 2] << 8) | buf[o + 3];
		return u32(buf, o + 4);
	}
	parts.hi = 0;
	if (size === 1) return b;
	if (size === 2) return (b << 8) | buf[o + 1];
	return (b << 24) | (buf[o + 1] << 16) | (buf[o + 2] << 8) | buf[o + 3];
}

/** The size of `hi`/`lo` as a QUIC varint, throwing at 2^62 or above. */
export function lengthQuic(hi: number, lo: number): number {
	if (hi === 0) {
		if (lo < 0x40) return 1;
		if (lo < 0x4000) return 2;
		if (lo < 0x4000_0000) return 4;
	}
	if (hi >= 0x4000_0000) throw new RangeError(`overflow, value larger than 62-bits: ${toBigInt(hi, lo)}`);
	return 8;
}

/** Encode a QUIC varint of {@link lengthQuic} `size` at the start of `dst`. */
export function writeQuic(dst: Uint8Array, hi: number, lo: number, size: number) {
	if (size === 1) {
		dst[0] = lo;
	} else if (size === 2) {
		dst[0] = 0x40 | (lo >>> 8);
		dst[1] = lo;
	} else if (size === 4) {
		setU32(dst, 0, 0x8000_0000 | lo);
	} else {
		setU32(dst, 0, 0xc000_0000 | hi);
		setU32(dst, 4, lo);
	}
}

/** The size of a leading-ones varint, from its first byte. */
export function peekLeadingOnes(first: number): number {
	return Math.clz32(~(first << 24)) + 1;
}

/** Decode a leading-ones varint of `size` bytes at `o`, returning the lower half and setting `parts.hi`. The bytes must be there. */
export function readLeadingOnes(buf: Uint8Array, o: number, size: number): number {
	const b = buf[o];
	switch (size) {
		case 1:
			parts.hi = 0;
			return b;
		case 2:
			parts.hi = 0;
			return ((b & 0x3f) << 8) | buf[o + 1];
		case 3:
			parts.hi = 0;
			return ((b & 0x1f) << 16) | (buf[o + 1] << 8) | buf[o + 2];
		case 4:
			parts.hi = 0;
			return ((b & 0x0f) << 24) | (buf[o + 1] << 16) | (buf[o + 2] << 8) | buf[o + 3];
		case 5:
			parts.hi = b & 0x07;
			return u32(buf, o + 1);
		case 6:
			parts.hi = ((b & 0x03) << 8) | buf[o + 1];
			return u32(buf, o + 2);
		case 7:
			parts.hi = ((b & 0x01) << 16) | (buf[o + 1] << 8) | buf[o + 2];
			return u32(buf, o + 3);
		case 8:
			parts.hi = (buf[o + 1] << 16) | (buf[o + 2] << 8) | buf[o + 3];
			return u32(buf, o + 4);
		default:
			parts.hi = u32(buf, o + 1);
			return u32(buf, o + 5);
	}
}

/**
 * The size of `hi`/`lo` as a leading-ones varint, in its shortest form. The 7-byte form is skipped:
 * draft-17 rejects it, and the 8-byte form is valid everywhere.
 */
export function lengthLeadingOnes(hi: number, lo: number): number {
	if (hi === 0) {
		if (lo < 0x80) return 1;
		if (lo < 0x4000) return 2;
		if (lo < 0x20_0000) return 3;
		if (lo < 0x1000_0000) return 4;
	}
	if (hi < 0x8) return 5;
	if (hi < 0x400) return 6;
	if (hi < 0x100_0000) return 8;
	return 9;
}

/** Encode a leading-ones varint of {@link lengthLeadingOnes} `size` at the start of `dst`. */
export function writeLeadingOnes(dst: Uint8Array, hi: number, lo: number, size: number) {
	switch (size) {
		case 1:
			dst[0] = lo;
			return;
		case 2:
			dst[0] = 0x80 | (lo >>> 8);
			dst[1] = lo;
			return;
		case 3:
			dst[0] = 0xc0 | (lo >>> 16);
			dst[1] = lo >>> 8;
			dst[2] = lo;
			return;
		case 4:
			setU32(dst, 0, 0xe000_0000 | lo);
			return;
		case 5:
			dst[0] = 0xf0 | hi;
			setU32(dst, 1, lo);
			return;
		case 6:
			dst[0] = 0xf8 | (hi >>> 8);
			dst[1] = hi;
			setU32(dst, 2, lo);
			return;
		case 8:
			setU32(dst, 0, 0xfe00_0000 | hi);
			setU32(dst, 4, lo);
			return;
		default:
			dst[0] = 0xff;
			setU32(dst, 1, hi);
			setU32(dst, 5, lo);
	}
}
