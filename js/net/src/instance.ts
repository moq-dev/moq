/**
 * Stream instances: a string naming one continuous stream at a path. Within one, group
 * sequences never restart and never name two different groups, so a reader continues across
 * every route that shares it; different instances are never stitched together.
 * Mirrors `Instance` in rs/moq-net.
 *
 * @module
 */

/** A validated instance: any non-empty text. */
export type Valid = string & { readonly _brand: "instance" };

/** What every {@link isMinted minted} instance starts with. */
const MINTED = "~";

/** Parse instance text, refusing an empty one. */
export function parse(text: string): Valid {
	if (text.length === 0) throw new RangeError("invalid instance: empty");
	return text as Valid;
}

/** A fresh random instance, for a new stream: 128 random bits as hex. */
export function random(): Valid {
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("") as Valid;
}

/** A fresh instance a session mints for a route its wire delivered without one. @internal */
export function minted(): Valid {
	return `${MINTED}${random()}` as Valid;
}

/**
 * Whether a relay minted this instance, for a route whose wire (an older moq-lite, or
 * moq-transport) carries none. It names the announcement rather than the publisher's stream,
 * so it can neither match nor contradict an instance the publisher named elsewhere, such as in
 * a catalog. Hops that forward it keep the mark.
 */
export function isMinted(instance: Valid): boolean {
	return instance.startsWith(MINTED);
}
