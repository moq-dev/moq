/**
 * Stream identities: a string naming one continuous stream at a path. Within one, group
 * sequences never restart and never name two different groups, so a reader continues across
 * every route that shares it; different identities are never stitched together.
 * Mirrors `Identity` in rs/moq-net.
 *
 * @module
 */

/** A validated identity: any non-empty text. */
export type Valid = string & { readonly _brand: "identity" };

/** What every {@link isMinted minted} identity starts with. */
const MINTED = "~";

/** Parse identity text, refusing an empty one. */
export function parse(text: string): Valid {
	if (text.length === 0) throw new RangeError("invalid identity: empty");
	return text as Valid;
}

/** A fresh random identity, for a new stream: 128 random bits as hex. */
export function random(): Valid {
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("") as Valid;
}

/** A fresh identity a session mints for a route its wire delivered without one. @internal */
export function minted(): Valid {
	return `${MINTED}${random()}` as Valid;
}

/**
 * Whether a relay minted this identity, for a route whose wire (an older moq-lite, or
 * moq-transport) carries none. It names the announcement rather than the publisher's stream,
 * so it can neither match nor contradict an identity the publisher named elsewhere, such as in
 * a catalog. Hops that forward it keep the mark.
 */
export function isMinted(identity: Valid): boolean {
	return identity.startsWith(MINTED);
}
