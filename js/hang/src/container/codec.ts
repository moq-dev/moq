import type { Time } from "@moq/lite";
import type * as Catalog from "../catalog";

/**
 * Encodes a timestamp according to the specified container format.
 *
 * @param timestamp - The timestamp in microseconds
 * @param container - The container format to use
 * @returns The encoded timestamp as a Uint8Array
 */
export function encodeTimestamp(timestamp: Time.Micro, container: Catalog.Container = { kind: "legacy" }): Uint8Array {
	switch (container.kind) {
		case "legacy":
			return encodeVarInt(timestamp);
		case "cmaf":
			throw new Error("cmaf container uses moof+mdat, not direct timestamp encoding");
		default:
			throw new Error(`unsupported container kind: ${(container as { kind: string }).kind}`);
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
	container: Catalog.Container = { kind: "legacy" },
): [Time.Micro, Uint8Array] {
	switch (container.kind) {
		case "legacy": {
			const [value, remaining] = decodeVarInt(buffer);
			return [value as Time.Micro, remaining];
		}
		case "cmaf":
			throw new Error("cmaf container uses moof+mdat, not direct timestamp decoding");
		default:
			throw new Error(`unsupported container kind: ${(container as { kind: string }).kind}`);
	}
}

/**
 * Gets the size in bytes of an encoded timestamp for the given container format.
 * For variable-length formats, returns the maximum size.
 *
 * @param container - The container format
 * @returns Size in bytes
 */
export function getTimestampSize(container: Catalog.Container = { kind: "legacy" }): number {
	switch (container.kind) {
		case "legacy":
			return 8; // VarInt maximum size
		case "cmaf":
			throw new Error("cmaf container uses moof+mdat, not direct timestamp encoding");
		default:
			throw new Error(`unsupported container kind: ${(container as { kind: string }).kind}`);
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
