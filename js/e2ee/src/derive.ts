import { Failure } from "./failure.ts";
import { KEY_LABEL, MAX_BYTES, MAX_U32, NAME_LABEL, NAME_LEN, PATH_LABEL } from "./limits.ts";

/** An opaque physical track name: 22 unpadded base64url characters derived from a semantic name. */
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
export function checkU32(value: number): void {
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

/** `"moq-e2ee-00 name" || bytes(context) || bytes(epoch) || u64(kid) || bytes(semantic)`. */
export function nameInfo(context: Uint8Array, epoch: string, kid: number, semantic: string): Uint8Array<ArrayBuffer> {
	return new Info().label(NAME_LABEL).bytes(context).text(epoch).u64(kid).text(semantic).finish();
}

/** `"moq-e2ee-00 key" || bytes(context) || bytes(epoch) || u64(kid) || bytes(name) || domain`. */
export function keyInfo(
	context: Uint8Array,
	epoch: string,
	kid: number,
	name: Name,
	domain: Domain,
): Uint8Array<ArrayBuffer> {
	return new Info().label(KEY_LABEL).bytes(context).text(epoch).u64(kid).text(name).u8(domain).finish();
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

/** Encode 16 bytes of HKDF output as unpadded base64url. */
export function encode(material: Uint8Array): Name {
	if (material.byteLength !== NAME_LEN) throw new Failure("identity");
	const text = btoa(String.fromCharCode(...material));
	return text.replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "") as Name;
}

// 21 free characters, then one whose low four bits are zero: exactly 16 bytes, canonical.
const NAME = /^[A-Za-z0-9_-]{21}[AQgw]$/;

/** Accept a name learned from a track or a decrypted catalog. */
export function parse(name: string): Name {
	if (!NAME.test(name)) throw new Failure("identity");
	return name as Name;
}
