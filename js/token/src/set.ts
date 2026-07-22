/**
 * A JWK Set: a bag of keys, selected per token by its `kid` header.
 *
 * The counterpart of the Rust `moq-token` crate's `KeySet`, and wire-compatible with
 * it (RFC 7517 section 5).
 *
 * @module
 */

import * as jose from "jose";
import * as z from "zod/mini";
import type { Claims } from "./claims.ts";
import { type Key, KeySchema, type PublicKey, sign, toPublicKey, verify } from "./key.ts";

/**
 * A JWK Set.
 *
 * See <https://datatracker.ietf.org/doc/html/rfc7517#section-5>.
 */
export const KeySetSchema = z.object({
	/** The keys in the set, in preference order. */
	keys: z.array(KeySchema),
});

/** A set of keys, each optionally identified by a `kid`. */
export type KeySet = z.infer<typeof KeySetSchema>;

/** A set of keys with no private material, safe to publish as a JWKS endpoint. */
export interface PublicKeySet {
	/** The public keys in the set, in preference order. */
	keys: PublicKey[];
}

/** Parse a JWK Set from JSON. Throws when the JSON or any key in it is invalid. */
export function loadSet(jwks: string): KeySet {
	let data: unknown;
	try {
		data = JSON.parse(jwks.trim());
	} catch {
		throw new Error("Failed to parse JWKS: invalid JSON format");
	}

	try {
		return KeySetSchema.parse(data);
	} catch (error) {
		throw new Error(`Failed to validate JWKS: ${error instanceof Error ? error.message : "unknown error"}`);
	}
}

/** Strip the private material from every key in the set. Throws on a symmetric (oct) key, which has no public half. */
export function toPublicSet(set: KeySet): PublicKeySet {
	return { keys: set.keys.map(toPublicKey) };
}

/** The key with the given `kid`, or undefined when the set has no such key. */
export function findKey(set: KeySet, kid: string): Key | undefined {
	return set.keys.find((key) => key.kid === kid);
}

/** Sign the claims with the first key that permits signing and contains signing material. */
export async function signWith(set: KeySet, claims: Claims): Promise<string> {
	const key = set.keys.find((key) => key.key_ops.includes("sign") && (key.kty === "oct" || key.d !== undefined));
	if (!key) throw new Error("Cannot find signing key");
	return await sign(key, claims);
}

/**
 * Verify a token with the key matching its `kid` header, returning its claims.
 *
 * A token without a `kid` is accepted only when the set holds exactly one key.
 */
export async function verifyWith(set: KeySet, token: string): Promise<Claims> {
	let kid: string | undefined;
	try {
		kid = jose.decodeProtectedHeader(token).kid;
	} catch {
		throw new Error("Failed to decode token header");
	}

	let key: Key | undefined;
	if (kid !== undefined) {
		key = findKey(set, kid);
		if (!key) throw new Error(`Cannot find key with kid ${kid}`);
	} else if (set.keys.length === 1) {
		// No kid to match on, but there's only one key it could be.
		key = set.keys[0];
	} else {
		throw new Error("Missing kid in JWT header");
	}

	return await verify(key, token);
}
