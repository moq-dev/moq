import { Failure } from "./failure.ts";
import {
	KEY_LABEL,
	MAX_BYTES,
	MAX_U32,
	NAME_BLOCK,
	NAME_KEY_LABEL,
	NAME_MAC_LABEL,
	NAME_NONCE_LEN,
	PATH_LABEL,
	TAG_LEN,
} from "./limits.ts";

/** An opaque physical track name: unpadded base64url of a semantic name sealed under its generation. */
export type Name = string & { readonly _brand: "e2ee-name" };

/** Grouped-frame versus datagram key domain. */
export type Domain = 0x00 | 0x01;

/** The grouped-frame key domain. */
export const GROUP: Domain = 0x00;

/** The datagram key domain. */
export const DATAGRAM: Domain = 0x01;

const encoder = new TextEncoder();

/** Refuse a group or kid that is not an integer in `0..=2^53-1`. */
export function checkU53(value: number): void {
	if (!Number.isSafeInteger(value) || value < 0) throw new Failure("identity");
}

/** Refuse a frame that is not an integer in `0..=2^32-1`. */
function checkU32(value: number): void {
	if (!Number.isInteger(value) || value < 0 || value > MAX_U32) throw new Failure("identity");
}

/** Refuse a `bytes` field longer than 65535 bytes. */
export function checkBytes(length: number): void {
	if (length > MAX_BYTES) throw new Failure("identity");
}

// Concatenation of encoded HKDF info fields.
class Info {
	#parts: Uint8Array[] = [];
	#length = 0;

	label(label: string): this {
		return this.#push(encoder.encode(label));
	}

	// `u16(length) || data`.
	bytes(data: Uint8Array): this {
		checkBytes(data.byteLength);
		const length = new Uint8Array(2);
		new DataView(length.buffer).setUint16(0, data.byteLength);
		return this.#push(length).#push(data);
	}

	text(text: string): this {
		return this.bytes(encoder.encode(text));
	}

	u64(value: number): this {
		checkU53(value);
		const out = new Uint8Array(8);
		const view = new DataView(out.buffer);
		view.setUint32(0, Math.floor(value / 2 ** 32));
		view.setUint32(4, value >>> 0);
		return this.#push(out);
	}

	u8(value: number): this {
		return this.#push(Uint8Array.of(value));
	}

	finish(): Uint8Array<ArrayBuffer> {
		const out = new Uint8Array(this.#length);
		let offset = 0;
		for (const part of this.#parts) {
			out.set(part, offset);
			offset += part.byteLength;
		}
		return out;
	}

	#push(part: Uint8Array): this {
		this.#parts.push(part);
		this.#length += part.byteLength;
		return this;
	}
}

/** `"moq-e2ee-00 path" || bytes(context) || u64(kid) || bytes(semantic)`. */
export function pathInfo(context: Uint8Array, kid: number, semantic: string): Uint8Array<ArrayBuffer> {
	return new Info().label(PATH_LABEL).bytes(context).u64(kid).text(semantic).finish();
}

/** `"moq-e2ee-00 name-mac" || bytes(context) || bytes(epoch) || u64(kid)`. */
export function nameMacInfo(context: Uint8Array, epoch: string, kid: number): Uint8Array<ArrayBuffer> {
	return new Info().label(NAME_MAC_LABEL).bytes(context).text(epoch).u64(kid).finish();
}

/** `"moq-e2ee-00 name-key" || bytes(context) || bytes(epoch) || u64(kid)`. */
export function nameKeyInfo(context: Uint8Array, epoch: string, kid: number): Uint8Array<ArrayBuffer> {
	return new Info().label(NAME_KEY_LABEL).bytes(context).text(epoch).u64(kid).finish();
}

/** `"moq-e2ee-00 key" || bytes(context) || bytes(epoch) || u64(kid) || bytes(semantic) || domain`. */
export function keyInfo(
	context: Uint8Array,
	epoch: string,
	kid: number,
	semantic: string,
	domain: Domain,
): Uint8Array<ArrayBuffer> {
	return new Info().label(KEY_LABEL).bytes(context).text(epoch).u64(kid).text(semantic).u8(domain).finish();
}

/** `semantic || 0x80 || 0x00...`, with the fewest zeros that reach a multiple of 16 bytes. */
export function pad(semantic: string): Uint8Array<ArrayBuffer> {
	const bytes = encoder.encode(semantic);
	checkBytes(bytes.byteLength);
	const out = new Uint8Array((Math.floor(bytes.byteLength / NAME_BLOCK) + 1) * NAME_BLOCK);
	out.set(bytes);
	out[bytes.byteLength] = 0x80;
	return out;
}

const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Strip canonical padding and decode UTF-8; anything else is {@link Failure} `authentication`. */
export function unpad(padded: Uint8Array): string {
	let end = padded.byteLength - 1;
	while (end >= 0 && padded[end] === 0) end--;
	if (end < 0 || padded[end] !== 0x80 || padded.byteLength - end > NAME_BLOCK) throw new Failure("authentication");
	try {
		return decoder.decode(padded.subarray(0, end));
	} catch {
		throw new Failure("authentication");
	}
}

/** 96-bit AES-GCM nonce: `u64(group) || u32(frame)`. */
export function nonce(group: number, frame: number): Uint8Array<ArrayBuffer> {
	checkU53(group);
	checkU32(frame);
	const out = new Uint8Array(12);
	const view = new DataView(out.buffer);
	view.setUint32(0, Math.floor(group / 2 ** 32));
	view.setUint32(4, group >>> 0);
	view.setUint32(8, frame);
	return out;
}

/** Encode bytes as unpadded base64url. */
export function encode(material: Uint8Array): string {
	// Per byte rather than spread: a long name would overflow the argument limit.
	let binary = "";
	for (const byte of material) binary += String.fromCharCode(byte);
	return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

const BASE64URL = /^[A-Za-z0-9_-]*$/;

/**
 * Decode a physical name, checking syntax only: only its generation can open it.
 *
 * Throws {@link Failure} `identity` unless `name` is canonical unpadded base64url decoding to a
 * 12-byte nonce, a whole number of 16-byte blocks, and a 16-byte tag.
 */
export function decode(name: string): Uint8Array<ArrayBuffer> {
	if (!BASE64URL.test(name)) throw new Failure("identity");
	let binary: string;
	try {
		binary = atob(name.replaceAll("-", "+").replaceAll("_", "/"));
	} catch {
		throw new Failure("identity");
	}
	const material = Uint8Array.from(binary, (c) => c.charCodeAt(0));
	// Refuses nonzero trailing bits, so each name has one spelling.
	if (encode(material) !== name) throw new Failure("identity");
	const padded = material.byteLength - NAME_NONCE_LEN - TAG_LEN;
	if (padded < NAME_BLOCK || padded % NAME_BLOCK !== 0) throw new Failure("identity");
	return material;
}

/** Accept a physical name learned from a track request; see {@link decode}. */
export function parse(name: string): Name {
	decode(name);
	return name as Name;
}
