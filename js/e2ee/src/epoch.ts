/**
 * One publisher instance's generation epoch, the lowercase hyphenated text of a UUIDv7. The
 * instance announces it as its route's instance, so a subscriber reads it back from the
 * broadcast it resolved ({@link of}). A wire that carries no instance hands the subscriber one
 * its relay minted, which is no epoch: the application supplies the epoch over its own
 * authenticated channel then. Mirrors `Epoch` in rs/moq-e2ee.
 *
 * @module
 */

import type { Broadcast, Instance } from "@moq/net";
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

/** The route instance that announces `epoch`. */
export function instance(epoch: Valid): Instance.Valid {
	return epoch as string as Instance.Valid;
}

/** The epoch a resolved broadcast's route announced, if its instance is one. */
export function of(broadcast: Broadcast.Consumer): Valid | undefined {
	const instance = broadcast.instance;
	if (instance === undefined) return undefined;
	try {
		return parse(instance);
	} catch {
		return undefined;
	}
}
