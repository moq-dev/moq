/**
 * One publisher instance's generation epoch, the lowercase hyphenated text of a UUIDv7. The
 * instance announces it as its route's identity, so a subscriber reads it back from the
 * broadcast it resolved ({@link of}). A wire that carries no identity hands the subscriber one
 * its session minted, which is no epoch: the application supplies the epoch over its own
 * authenticated channel then. Mirrors `moq_e2ee::Epoch`.
 *
 * @module
 */

import { type Broadcast, Identity } from "@moq/net";
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

/** Mint a fresh epoch using the wall clock and secure randomness, ordered newest last. */
export function mint(): Valid {
	return parse(v7());
}

/** The route identity that announces `epoch`. */
export function identity(epoch: Valid): Identity.Valid {
	return epoch as string as Identity.Valid;
}

/** The epoch a resolved broadcast's route announced, if its identity is one. */
export function of(broadcast: Broadcast.Consumer): Valid | undefined {
	const identity = broadcast.identity;
	// One a session minted for a wire without identities names no epoch.
	if (identity === undefined || Identity.isMinted(identity)) return undefined;
	try {
		return parse(identity);
	} catch {
		return undefined;
	}
}
