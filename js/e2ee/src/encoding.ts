import { MAX_U32 } from "./constants.ts";
import { Failure } from "./error.ts";

const utf8 = new TextEncoder();

/** UTF-8 encode a string. */
export function encodeUtf8(value: string): Uint8Array {
	return utf8.encode(value);
}

/** Concatenate copies of `parts` into one buffer. */
export function concat(...parts: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

/** Copy `value` into a tight owned `ArrayBuffer` view WebCrypto will accept. */
export function copy(value: Uint8Array): Uint8Array<ArrayBuffer> {
	const out = new Uint8Array(value.byteLength);
	out.set(value);
	return out;
}

/** Refuse a non-integer or a value outside `0..=2^53-1`. */
export function checkU53(value: number, label: string): void {
	if (!Number.isSafeInteger(value) || value < 0) {
		throw new Failure("identity", `${label} is outside 0..=2^53-1`);
	}
}

/** Refuse a non-integer or a value outside `0..=2^32-1`. */
export function checkU32(value: number, label: string): void {
	if (!Number.isInteger(value) || value < 0 || value > MAX_U32) {
		throw new Failure("identity", `${label} is outside 0..=2^32-1`);
	}
}

/** Group (or datagram sequence) plus frame identity. */
export function checkIdentity(group: number, frame: number): void {
	checkU53(group, "group");
	checkU32(frame, "frame");
}

/** Big-endian u16. */
export function encodeU16(value: number): Uint8Array {
	if (!Number.isInteger(value) || value < 0 || value > 0xffff) {
		throw new Failure("identity", `u16 out of range: ${value}`);
	}
	return new Uint8Array([(value >> 8) & 0xff, value & 0xff]);
}

/** Big-endian u32. */
export function encodeU32(value: number): Uint8Array {
	checkU32(value, "u32");
	const out = new Uint8Array(4);
	new DataView(out.buffer).setUint32(0, value);
	return out;
}

/** Big-endian u64 of a safe integer. */
export function encodeU64(value: number): Uint8Array {
	checkU53(value, "u64");
	const out = new Uint8Array(8);
	new DataView(out.buffer).setBigUint64(0, BigInt(value));
	return out;
}

/** `u16(length) || data`, length at most 65535. */
export function encodeBytes(value: Uint8Array): Uint8Array {
	if (value.length > 0xffff) throw new Failure("identity", `bytes too long: ${value.length}`);
	return concat(encodeU16(value.length), value);
}

/** Unpadded base64url of `bytes`. */
export function base64url(bytes: Uint8Array): string {
	let binary = "";
	for (const b of bytes) binary += String.fromCharCode(b);
	return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

/** Hex encode. */
export function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Hex decode. */
export function unhex(value: string): Uint8Array {
	if (value.length % 2 !== 0) throw new Error(`odd hex length: ${value.length}`);
	const out = new Uint8Array(value.length / 2);
	for (let i = 0; i < out.length; i++) {
		out[i] = Number.parseInt(value.slice(i * 2, i * 2 + 2), 16);
	}
	return out;
}

/** AES-GCM nonce: `u64(group) || u32(frame)`. */
export function nonce(group: number, frame: number): Uint8Array {
	checkIdentity(group, frame);
	return concat(encodeU64(group), encodeU32(frame));
}

/** Normalize context bytes; a string is UTF-8. */
export function contextBytes(context: Uint8Array | string): Uint8Array {
	const bytes = typeof context === "string" ? encodeUtf8(context) : copy(context);
	if (bytes.length > 0xffff) throw new Failure("identity", "context exceeds 65535 bytes");
	return bytes;
}
