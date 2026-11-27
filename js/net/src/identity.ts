/**
 * Stream identities: one continuous stream at a path. Within one, group keys never
 * restart and never name two different groups, so a reader continues across every route that
 * shares it; different identities are never stitched together.
 * Mirrors `Identity` in rs/moq-net.
 *
 * @module
 */

import { hex, Minted } from "./mint.ts";

/** A named identity: its text, as a publisher or catalog names it. */
export type Named = string & { readonly _brand: "identity" };

/** An identity a session {@link isMinted minted}; see {@link Valid}. */
export type { Minted };

/**
 * A validated identity, compared with `===` and used as a map key. A named identity is its
 * text; one a session minted is an object no caller can build, interned so equal ones are the
 * same object, so the two kinds never match even with the same text. Read the text with
 * {@link text}.
 */
export type Valid = Named | Minted;

// The most UTF-8 bytes an identity holds: room for a public key in any common text encoding
// while bounding what a claim makes a relay hold. Mirrors `MAX_LEN` in rs/moq-net.
const MAX_BYTES = 256;
const encoder = new TextEncoder();

/**
 * Parse a named identity's text, refusing an empty one, one longer than 256 UTF-8 bytes, or an
 * ill-formed one (a lone surrogate, which UTF-8 cannot carry).
 */
export function parse(text: string): Named {
	if (text.length === 0) throw new RangeError("invalid identity: empty");
	if (!text.isWellFormed()) throw new RangeError("invalid identity: ill-formed text");
	// Every UTF-16 code unit is at least one UTF-8 byte and at most three, so only a length in
	// between needs encoding to tell.
	if (text.length > MAX_BYTES || (text.length * 3 > MAX_BYTES && encoder.encode(text).byteLength > MAX_BYTES)) {
		throw new RangeError("invalid identity: longer than 256 bytes");
	}
	return text as Named;
}

/** A fresh random identity, for a new stream: 128 random bits as hex. */
export function random(): Named {
	return hex() as Named;
}

/** The identity's text, which alone does not tell a minted identity from a named one. */
export function text(identity: Valid): string {
	return typeof identity === "string" ? identity : identity.text;
}

/**
 * Whether a session minted this identity, for a route whose wire (an older moq-lite, or
 * moq-transport) carries none. It names the announcement rather than the publisher's stream,
 * so it can neither match nor contradict an identity the publisher named elsewhere, such as in
 * a catalog. Hops that forward it keep the flag, and only a session sets it: {@link parse}
 * never builds a minted identity.
 */
export function isMinted(identity: Valid): identity is Minted {
	return Minted.is(identity);
}
