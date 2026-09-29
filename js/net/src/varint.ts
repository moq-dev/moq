/**
 * Variable-length integers: QUIC's (RFC 9000 Section 16) and moq-transport's leading-ones form.
 * https://www.rfc-editor.org/rfc/rfc9000#section-16
 *
 * @module
 */

import {
	lengthLeadingOnes,
	lengthQuic,
	POW32,
	parts,
	peekLeadingOnes,
	peekQuic,
	readLeadingOnes,
	readQuic,
	split,
	toBigInt,
	writeLeadingOnes,
	writeQuic,
} from "./util/varint.ts";

/** Largest value that fits in a 1-byte varint (6 bits). */
export const MAX_U6 = 2 ** 6 - 1;
/** Largest value that fits in a 2-byte varint (14 bits). */
export const MAX_U14 = 2 ** 14 - 1;
/** Largest value that fits in a 4-byte varint (30 bits). */
export const MAX_U30 = 2 ** 30 - 1;
/** Largest value representable without precision loss (`Number.MAX_SAFE_INTEGER`, 53 bits). */
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

// The size of the varint at the start of `buf`, throwing unless all of it is there.
function sizeOf(buf: Uint8Array, size: (first: number) => number): number {
	if (buf.length === 0) throw new Error("buffer is empty");
	const n = size(buf[0]);
	if (buf.length < n) throw new Error(`buffer too short: need ${n} bytes, have ${buf.length}`);
	return n;
}

/** Number of bytes needed to encode a value in the leading-ones varint format. */
export function sizeLeadingOnes(v: number | bigint): number {
	const lo = split(v);
	return lengthLeadingOnes(parts.hi, lo);
}

/** Encode a value in leading-ones varint format into the provided buffer, returning the written subarray. */
export function encodeLeadingOnesTo(dst: ArrayBuffer, v: number | bigint): Uint8Array {
	const lo = split(v);
	const buf = new Uint8Array(dst, 0, lengthLeadingOnes(parts.hi, lo));
	writeLeadingOnes(buf, parts.hi, lo, buf.length);
	return buf;
}

/** Encode a value in leading-ones varint format into a freshly allocated buffer. */
export function encodeLeadingOnes(v: number | bigint): Uint8Array {
	return encodeLeadingOnesTo(new ArrayBuffer(9), v);
}

/**
 * Decode a leading-ones varint, returning the value and the remaining buffer.
 *
 * Accepts the 7-byte form, which only draft-18+ allows, since there is no version here to check.
 */
export function decodeLeadingOnes(buf: Uint8Array): [bigint, Uint8Array] {
	const size = sizeOf(buf, peekLeadingOnes);
	const lo = readLeadingOnes(buf, 0, size);
	return [toBigInt(parts.hi, lo), buf.subarray(size)];
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

/**
 * Encodes a value as a QUIC variable-length integer into the provided buffer,
 * returning the written subarray.
 */
export function encodeTo(dst: ArrayBuffer, v: number | bigint): Uint8Array {
	const lo = split(v);
	const buf = new Uint8Array(dst, 0, lengthQuic(parts.hi, lo));
	writeQuic(buf, parts.hi, lo, buf.length);
	return buf;
}

/**
 * Encodes a value as a QUIC variable-length integer.
 * Returns a new Uint8Array containing the encoded bytes.
 */
export function encode(v: number | bigint): Uint8Array {
	return encodeTo(new ArrayBuffer(8), v);
}

/**
 * Decodes a QUIC variable-length integer exactly from a buffer.
 * Returns a tuple of [value, remaining buffer].
 */
export function decodeBigInt(buf: Uint8Array): [bigint, Uint8Array] {
	const size = sizeOf(buf, peekQuic);
	const lo = readQuic(buf, 0, size);
	return [toBigInt(parts.hi, lo), buf.subarray(size)];
}

/**
 * Decodes a QUIC variable-length integer from a buffer.
 * Values above 53 bits lose precision; use {@link decodeBigInt} for exact decoding.
 */
export function decode(buf: Uint8Array): [number, Uint8Array] {
	const size = sizeOf(buf, peekQuic);
	const lo = readQuic(buf, 0, size);
	return [parts.hi * POW32 + lo, buf.subarray(size)];
}
