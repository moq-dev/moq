/** Publisher instance identities, encoded as lowercase hyphenated UUIDv7 text. @module */

import { v7 } from "uuid";

/** A validated UUIDv7, ordered newest last by ordinary string comparison. */
export type Valid = string & { readonly _brand: "epoch" };

const canonical = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** Parse UUID text without the path's `@` marker, refusing noncanonical forms. */
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
