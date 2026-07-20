/**
 * The payload of a token: a root, plus the publish/subscribe prefixes granted beneath it.
 *
 * @module
 */

import * as z from "zod/mini";
import * as Path from "./path.ts";

/** A `put`/`get` claim is one path or many; normalize it to a list. */
function list(claim: string | string[] | undefined): string[] {
	if (claim === undefined) return [];
	return typeof claim === "string" ? [claim] : claim;
}

/**
 * The immutable ceiling on what a key may grant, embedded in its JWK.
 *
 * `root` is optional on the wire to match the Rust `moq-token` crate, which omits it
 * when the scope sits at the top level.
 */
export const ScopeSchema = z
	.object({
		/** The root that `put` and `get` are relative to. Defaults to the empty string. */
		root: z._default(z.string(), ""),
		/** Prefixes this key may grant to publishers, relative to `root`. */
		put: z.optional(z.array(z.string())),
		/** Prefixes this key may grant to subscribers, relative to `root`. */
		get: z.optional(z.array(z.string())),
	})
	.check(
		z.refine((data) => (data.put?.length ?? 0) > 0 || (data.get?.length ?? 0) > 0, {
			message: "Either put or get must contain at least one prefix",
		}),
	);

export type Scope = z.infer<typeof ScopeSchema>;

/**
 * The JWT claims structure for moq-token.
 *
 * `root` is optional on the wire: a token scoped to the top-level path omits it, so
 * it defaults to the empty string to match the Rust `moq-token` crate.
 */
export const ClaimsSchema = z
	.object({
		/** The root that `put` and `get` are relative to. Defaults to the empty string. */
		root: z._default(z.string(), ""),
		/** Paths the holder may publish to, relative to `root`. */
		put: z.optional(z.union([z.string(), z.array(z.string())])),
		/** Paths the holder may subscribe to, relative to `root`. Named `get` because `sub` is a reserved JWT claim. */
		get: z.optional(z.union([z.string(), z.array(z.string())])),
		/** Expiration time, as a unix timestamp in seconds. */
		exp: z.optional(z.number()),
		/** Issued-at time, as a unix timestamp in seconds. */
		iat: z.optional(z.number()),
	})
	.check(
		// Emptiness, not just presence: `put: []` grants nothing, and the Rust crate
		// rejects such a token as useless. Checking `!== undefined` here would mint
		// tokens that Rust then refuses to verify.
		z.refine((data) => list(data.put).length > 0 || list(data.get).length > 0, {
			message: "Either put or get must grant at least one path",
		}),
	);

/**
 * JWT claims structure for moq-token
 */
export type Claims = z.infer<typeof ClaimsSchema>;

/**
 * The access a {@link Claims} grants at a specific path, with every prefix rebased so
 * it is relative to that path.
 *
 * Produced by {@link authorize}. An empty string grants the path itself and everything
 * beneath it.
 */
export interface Permissions {
	/** Paths the holder may subscribe to, relative to the authorized path. */
	subscribe: string[];
	/** Paths the holder may publish to, relative to the authorized path. */
	publish: string[];
}

/**
 * The access `claims` grants at `path`, rebased so each returned prefix is relative
 * to `path`.
 *
 * `path` and `claims.root` must overlap, in either direction:
 *
 * - `path` extends the root (root `demo`, path `demo/room`), so the extra `room`
 *   narrows each prefix and drops the ones outside it.
 * - `path` is a parent of the root (root `demo`, path ``), so `demo` is prepended to
 *   each prefix to keep it anchored where the token points.
 *
 * Matching is segment-aware, so a root of `foo` does not cover `foobar`. Slashes at
 * the boundaries are implicit: `/demo/` and `demo` are the same path.
 *
 * Throws when the two don't overlap, and when they do but every prefix falls outside
 * `path`.
 *
 * This is authorization only. Verify the signature first with {@link verify}, which is
 * where expiry is enforced.
 *
 * @public
 */
export function authorize(claims: Claims, path: string): Permissions {
	const target = Path.normalize(path);
	const root = Path.normalize(claims.root);

	// Exactly one of these is non-empty: `suffix` is how far the path reaches past
	// the root, `prefix` is how far the root reaches past the path.
	let suffix: string;
	let prefix: string;

	const beyondRoot = Path.stripPrefix(target, root);
	const beyondPath = Path.stripPrefix(root, target);
	if (beyondRoot !== undefined) {
		[suffix, prefix] = [beyondRoot, ""];
	} else if (beyondPath !== undefined) {
		[suffix, prefix] = ["", beyondPath];
	} else {
		throw new Error(`path "${target}" does not overlap the token root "${root}"`);
	}

	const scope = (claim: string | string[] | undefined): string[] => {
		const scoped: string[] = [];
		for (const granted of list(claim)) {
			const full = Path.join(prefix, Path.normalize(granted));

			const remaining = Path.stripPrefix(full, suffix);
			if (remaining !== undefined) {
				// The grant covers the path; keep what's left below it.
				scoped.push(remaining);
			} else if (Path.hasPrefix(suffix, full)) {
				// The grant stops short of the path but still contains it, so
				// everything below the path is granted.
				scoped.push("");
			}
		}
		return scoped;
	};

	const permissions: Permissions = { subscribe: scope(claims.get), publish: scope(claims.put) };
	if (permissions.subscribe.length === 0 && permissions.publish.length === 0) {
		throw new Error(`token grants no access to path "${target}"`);
	}

	return permissions;
}

/**
 * Whether every path `claims` grants is covered by `scope`, per role.
 *
 * Both sides are resolved against their own root before comparing, so the same
 * grant expressed as `root: "demo"` + `put: ["room"]` or as `put: ["demo/room"]` is
 * treated identically. Matching is segment-aware, so a scope of `live` does not
 * cover `lively`, and the roles are checked independently: a publish-only scope
 * never authorizes a subscribe grant.
 *
 * Must stay in lockstep with `Scope::allows` in the Rust `moq-token` crate, which
 * checks the same keys.
 */
export function scopeAllows(scope: Scope, claims: Claims): boolean {
	return (
		covers(scope.root, scope.put ?? [], claims.root, list(claims.put)) &&
		covers(scope.root, scope.get ?? [], claims.root, list(claims.get))
	);
}

/**
 * Whether every `requested` path (relative to `claimsRoot`) sits beneath some
 * `granted` prefix (relative to `scopeRoot`).
 *
 * An empty `granted` denies everything, which is what makes a publish-only scope
 * reject subscribe grants rather than ignoring them.
 */
function covers(scopeRoot: string, granted: string[], claimsRoot: string, requested: string[]): boolean {
	const absolute = (root: string, relative: string) => Path.join(Path.normalize(root), Path.normalize(relative));

	return requested.every((request) => {
		const path = absolute(claimsRoot, request);
		return granted.some((grant) => Path.hasPrefix(path, absolute(scopeRoot, grant)));
	});
}
