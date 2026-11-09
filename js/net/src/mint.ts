/**
 * Minting instances, which only a session does. Not exported from the package entrypoint, so
 * the public API builds only named instances.
 *
 * @module
 */

import type { Valid } from "./instance.ts";

/**
 * Prefixes a minted instance's key. Well-formed text never starts with a low surrogate (one
 * only ever follows a high surrogate), so no named instance starts with it and `parse` refuses
 * a minted key, while equality and map keys stay plain string comparisons.
 */
export const MARK = "\uDC00";

/** 128 random bits as hex. */
export function hex(): string {
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** A fresh instance a session mints for a route its wire delivered without one. */
export function fresh(): Valid {
	return `${MARK}${hex()}` as Valid;
}

/** The minted instance with `named`'s text, for one a lite-07 peer flagged minted. */
export function mark(named: Valid): Valid {
	return `${MARK}${named}` as Valid;
}
