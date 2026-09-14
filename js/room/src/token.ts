/**
 * The LiveKit AccessToken analogue: minting a moq-auth token rooted at the room prefix.
 *
 * This package has no service and no storage. Joining a room is signing a token
 * with these claims and dialing the relay at that root. Sign it with `@moq/auth`.
 *
 * @module
 */

import { Path } from "@moq/net";

/** Claims a room participant should present. Compatible with `@moq/auth` `Claims`. */
export type Claims = {
	/** Room prefix. Broadcast paths are relative to this. */
	root: string;
	/** Subscribe to every broadcast in the room. */
	subscribe: string[];
	/** Publish only under this participant's identity. */
	publish: string[];
};

/**
 * Token claims for a participant in `room`.
 *
 * `root` is the room prefix, `subscribe` is `**` (everything under the room), and
 * `publish` is `<identity>/**` so a participant cannot publish at anyone else's
 * paths. Empty identities are rejected after normalization.
 */
export function claims(room: string, identity: string): Claims {
	identity = Path.from(identity);
	if (!identity) throw new Error("participant identity must not be empty");
	return {
		root: room,
		subscribe: ["**"],
		publish: [`${identity}/**`],
	};
}
