/** Publisher instance identities, encoded as lowercase hyphenated UUIDv7 text. @module */

import { v7 } from "uuid";

/** A validated UUIDv7, ordered newest last by ordinary string comparison. */
export type Valid = string & { readonly _brand: "epoch" };

const canonical = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** Parse UUID text, refusing noncanonical forms. */
export function parse(text: string): Valid {
	if (text.length !== 36 || !canonical.test(text))
		throw new RangeError("invalid epoch: expected a lowercase hyphenated UUIDv7");
	return text as Valid;
}

/** Mint a fresh identity using the wall clock and secure randomness, ordered newest last. */
export function mint(): Valid {
	return parse(v7());
}

/** Read the wall-clock time encoded in the UUID, with millisecond precision. */
export function time(epoch: Valid): Date {
	return new Date(Number.parseInt(epoch.slice(0, 8) + epoch.slice(9, 13), 16));
}

/** The UUID's 16 bytes, as the wire carries them. */
export function toBytes(epoch: Valid): Uint8Array {
	const hex = epoch.replaceAll("-", "");
	const bytes = new Uint8Array(16);
	for (let i = 0; i < 16; i++) bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
	return bytes;
}

/** Parse the wire's 16 bytes, refusing anything but a UUIDv7 with the RFC variant. */
export function fromBytes(bytes: Uint8Array): Valid {
	if (bytes.length !== 16) throw new RangeError("invalid epoch: expected 16 bytes");
	const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
	return parse(`${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`);
}
