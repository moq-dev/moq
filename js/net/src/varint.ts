/**
 * Variable-length integers: QUIC's (RFC 9000 Section 16) and moq-transport's leading-ones form.
 * https://www.rfc-editor.org/rfc/rfc9000#section-16
 *
 * @module
 */

import { toBigInt, toNumber } from "./util/u64.ts";
import {
	lengthLeadingOnes,
	lengthQuic,
	parts,
	peekLeadingOnes,
	peekQuic,
	readLeadingOnes,
	readQuic,
	split,
	writeLeadingOnes,
	writeQuic,
} from "./util/varint.ts";

/** Largest value representable without precision loss (`Number.MAX_SAFE_INTEGER`, 53 bits). @internal */
export const MAX_U53 = Number.MAX_SAFE_INTEGER;

// The size of the varint at the start of `buf`, throwing unless all of it is there.
function sizeOf(buf: Uint8Array, size: (first: number) => number): number {
	if (buf.length === 0) throw new Error("buffer is empty");
	const n = size(buf[0]);
	if (buf.length < n) throw new Error(`buffer too short: need ${n} bytes, have ${buf.length}`);
	return n;
}

/** Encode a value in leading-ones varint format into the provided buffer, returning the written subarray. @internal */
export function encodeLeadingOnesTo(dst: ArrayBuffer, v: number | bigint): Uint8Array {
	const lo = split(v);
	const buf = new Uint8Array(dst, 0, lengthLeadingOnes(parts.hi, lo));
	writeLeadingOnes(buf, parts.hi, lo, buf.length);
	return buf;
}

/** Encode a value in leading-ones varint format into a freshly allocated buffer. @internal */
export function encodeLeadingOnes(v: number | bigint): Uint8Array {
	return encodeLeadingOnesTo(new ArrayBuffer(9), v);
}

/**
 * Decode a leading-ones varint, returning the value and the remaining buffer.
 *
 * Accepts the 7-byte form, which only draft-18+ allows, since there is no version here to check.
 *
 * @internal
 */
export function decodeLeadingOnes(buf: Uint8Array): [bigint, Uint8Array] {
	const size = sizeOf(buf, peekLeadingOnes);
	const lo = readLeadingOnes(buf, 0, size);
	return [toBigInt(parts.hi, lo), buf.subarray(size)];
}

/**
 * Encodes a value as a QUIC variable-length integer into the provided buffer,
 * returning the written subarray.
 *
 * @internal
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
 * Throws above `Number.MAX_SAFE_INTEGER` rather than rounding; use {@link decodeBigInt} for those.
 */
export function decode(buf: Uint8Array): [number, Uint8Array] {
	const size = sizeOf(buf, peekQuic);
	const lo = readQuic(buf, 0, size);
	return [toNumber(parts.hi, lo), buf.subarray(size)];
}
