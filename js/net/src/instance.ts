/**
 * Stream instances: a string naming one continuous stream at a path. Within one, group
 * sequences never restart and never name two different groups, so a reader continues across
 * every route that shares it; different instances are never stitched together.
 * Mirrors `Instance` in rs/moq-net.
 *
 * @module
 */

import { hex, MARK } from "./mint.ts";

/**
 * A validated instance, compared with `===` and used as a map key. A named instance is its
 * text; one a session {@link isMinted minted} is keyed apart, so the two never match even with
 * the same text. Read the text with {@link text}.
 */
export type Valid = string & { readonly _brand: "instance" };

/** Parse a named instance's text, refusing an empty or ill-formed one (a lone surrogate, which UTF-8 cannot carry). */
export function parse(text: string): Valid {
	if (text.length === 0) throw new RangeError("invalid instance: empty");
	if (!text.isWellFormed()) throw new RangeError("invalid instance: ill-formed text");
	return text as Valid;
}

/** A fresh random instance, for a new stream: 128 random bits as hex. */
export function random(): Valid {
	return hex() as Valid;
}

/** The instance's text, which alone does not tell a minted instance from a named one. */
export function text(instance: Valid): string {
	return isMinted(instance) ? instance.slice(MARK.length) : instance;
}

/**
 * Whether a session minted this instance, for a route whose wire (an older moq-lite, or
 * moq-transport) carries none. It names the announcement rather than the publisher's stream,
 * so it can neither match nor contradict an instance the publisher named elsewhere, such as in
 * a catalog. Hops that forward it keep the flag, and only a session sets it: {@link parse}
 * never builds a minted instance.
 */
export function isMinted(instance: Valid): boolean {
	return instance.startsWith(MARK);
}
