import type * as Catalog from "../catalog";
import { DEFAULT_CONTAINER } from "../catalog";
import type * as Time from "../time";

/**
 * Encodes a timestamp according to the specified container format.
 *
 * @param timestamp - The timestamp in microseconds
 * @param container - The container format to use
 * @returns The encoded timestamp as a Uint8Array
 */
export function encodeTimestamp(timestamp: Time.Micro, container: Catalog.Container = DEFAULT_CONTAINER): Uint8Array {
	switch (container) {
		case "legacy":
			return encodeVarInt(timestamp);
		case "raw":
			return encodeU64(timestamp);
		case "fmp4":
			throw new Error("fmp4 container not yet implemented");
	}
}

/**
 * Decodes a timestamp from a buffer according to the specified container format.
 *
 * @param buffer - The buffer containing the encoded timestamp
 * @param container - The container format to use
 * @returns [timestamp in microseconds, remaining buffer after timestamp]
 */
export function decodeTimestamp(
	buffer: Uint8Array,
	container: Catalog.Container = DEFAULT_CONTAINER,
): [Time.Micro, Uint8Array] {
	switch (container) {
		case "legacy": {
			const [value, remaining] = decodeVarInt(buffer);
			return [value as Time.Micro, remaining];
		}
		case "raw": {
			const [value, remaining] = decodeU64(buffer);
			return [value as Time.Micro, remaining];
		}
		case "fmp4":
			throw new Error("fmp4 container not yet implemented");
	}
}

/**
 * Gets the size in bytes of an encoded timestamp for the given container format.
 * For variable-length formats, returns the maximum size.
 *
 * @param container - The container format
 * @returns Size in bytes
 */
export function getTimestampSize(container: Catalog.Container = DEFAULT_CONTAINER): number {
	switch (container) {
		case "legacy":
			return 8; // VarInt maximum size
		case "raw":
			return 8; // u64 fixed size
		case "fmp4":
			throw new Error("fmp4 container not yet implemented");
	}
}

// ============================================================================
// LEGACY VARINT IMPLEMENTATION
// ============================================================================

const MAX_U6 = 2 ** 6 - 1;
const MAX_U14 = 2 ** 14 - 1;
const MAX_U30 = 2 ** 30 - 1;
const MAX_U53 = Number.MAX_SAFE_INTEGER;

function decodeVarInt(buf: Uint8Array): [number, Uint8Array] {
	const size = 1 << ((buf[0] & 0xc0) >> 6);

	const view = new DataView(buf.buffer, buf.byteOffset, size);
	const remain = new Uint8Array(buf.buffer, buf.byteOffset + size, buf.byteLength - size);
	let v: number;

	if (size === 1) {
		v = buf[0] & 0x3f;
	} else if (size === 2) {
		v = view.getUint16(0) & 0x3fff;
	} else if (size === 4) {
		v = view.getUint32(0) & 0x3fffffff;
	} else if (size === 8) {
		// NOTE: Precision loss above 2^52
		v = Number(view.getBigUint64(0) & 0x3fffffffffffffffn);
	} else {
		throw new Error("impossible");
	}

	return [v, remain];
}

function encodeVarInt(v: number): Uint8Array {
	const dst = new Uint8Array(8);

	if (v <= MAX_U6) {
		dst[0] = v;
		return new Uint8Array(dst.buffer, dst.byteOffset, 1);
	}

	if (v <= MAX_U14) {
		const view = new DataView(dst.buffer, dst.byteOffset, 2);
		view.setUint16(0, v | 0x4000);
		return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
	}

	if (v <= MAX_U30) {
		const view = new DataView(dst.buffer, dst.byteOffset, 4);
		view.setUint32(0, v | 0x80000000);
		return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
	}

	if (v <= MAX_U53) {
		const view = new DataView(dst.buffer, dst.byteOffset, 8);
		view.setBigUint64(0, BigInt(v) | 0xc000000000000000n);
		return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
	}

	throw new Error(`overflow, value larger than 53-bits: ${v}`);
}

// ============================================================================
// RAW U64 IMPLEMENTATION
// ============================================================================

/**
 * Decodes a fixed 8-byte big-endian unsigned 64-bit integer.
 */
function decodeU64(buf: Uint8Array): [number, Uint8Array] {
	if (buf.byteLength < 8) {
		throw new Error("Buffer too short for u64 decode");
	}

	const view = new DataView(buf.buffer, buf.byteOffset, 8);
	const value = Number(view.getBigUint64(0));
	const remain = new Uint8Array(buf.buffer, buf.byteOffset + 8, buf.byteLength - 8);

	return [value, remain];
}

/**
 * Encodes a number as a fixed 8-byte big-endian unsigned 64-bit integer.
 * Much simpler than VarInt!
 */
function encodeU64(v: number): Uint8Array {
	const dst = new Uint8Array(8);
	const view = new DataView(dst.buffer, dst.byteOffset, 8);
	view.setBigUint64(0, BigInt(v));
	return dst;
}
