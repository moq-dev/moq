/**
 * The payload of a token: a root, plus the publish/subscribe patterns granted beneath it.
 *
 * @module
 */

import { Pattern, Patterns } from "@moq/pattern";
import * as z from "@zod/mini";
import * as Path from "./path.ts";
import { decodeGrants } from "./wire.ts";

/** A list of pattern texts, each validated by `Pattern.parse`. */
export const PatternListSchema = z.array(
	z.string().check(
		z.refine(
			(text) => {
				try {
					Pattern.parse(text);
					return true;
				} catch {
					return false;
				}
			},
			{ message: "invalid path pattern" },
		),
	),
);

/** Parse a list of pattern texts into a reduced union. */
export function patterns(texts: readonly string[] | undefined): Patterns {
	return new Patterns((texts ?? []).map((text) => Pattern.parse(text)));
}

const ScopeFields = {
	/** The root that `publish` and `subscribe` are relative to. Defaults to the empty string. */
	root: z._default(z.string(), ""),
	/** Patterns this key may grant to publishers, relative to `root`. */
	publish: z.optional(PatternListSchema),
	/** Patterns this key may grant to subscribers, relative to `root`. */
	subscribe: z.optional(PatternListSchema),
};

/**
 * The immutable ceiling on what a key may grant, embedded in its JWK.
 *
 * `root` is optional on the wire to match the Rust `moq-auth` crate, which omits it
 * when the scope sits at the top level. A legacy `put`/`get` prefix scope reads as the
 * subtree patterns it meant. Any other field is refused.
 */
export const ScopeSchema = z
	.pipe(
		z.strictObject({
			...ScopeFields,
			put: z.optional(z.array(z.string())),
			get: z.optional(z.array(z.string())),
		}),
		z.transform(decodeGrants),
	)
	.check(
		z.refine((data) => (data.publish?.length ?? 0) > 0 || (data.subscribe?.length ?? 0) > 0, {
			message: "Either publish or subscribe must contain at least one pattern",
		}),
	);

export type Scope = z.output<typeof ScopeSchema>;

const ClaimsFields = {
	...ScopeFields,
	/** Expiration time, as a whole unix timestamp in seconds. */
	exp: z.optional(z.int()),
	/** Issued-at time, as a whole unix timestamp in seconds. */
	iat: z.optional(z.int()),
};

const PrefixListSchema = z.union([z.string(), z.array(z.string())]);

/**
 * The JWT claims structure for moq-auth.
 *
 * `root` is optional on the wire: a token scoped to the top-level path omits it, so
 * it defaults to the empty string to match the Rust `moq-auth` crate. A pattern names
 * exactly what it says: `alice` is one broadcast, `alice/**` is a subtree, and `**` is
 * everything under the root. Legacy `moq-token` claims read too, each `put`/`get`
 * prefix `p` as the subtree `p/**`, and signing writes that form whenever it says the
 * same thing. Any other field fails verification.
 */
export const ClaimsSchema = z
	.pipe(
		z.strictObject({ ...ClaimsFields, put: z.optional(PrefixListSchema), get: z.optional(PrefixListSchema) }),
		z.transform(decodeGrants),
	)
	.check(
		// Emptiness, not just presence: `publish: []` grants nothing, and the Rust crate
		// rejects such a token as useless. Checking `!== undefined` here would mint
		// tokens that Rust then refuses to verify.
		z.refine((data) => (data.publish?.length ?? 0) > 0 || (data.subscribe?.length ?? 0) > 0, {
			message: "Either publish or subscribe must grant at least one pattern",
		}),
	);

/**
 * JWT claims structure for moq-auth
 */
export type Claims = z.output<typeof ClaimsSchema>;

/**
 * The access a {@link Claims} grants at a specific path, with every pattern rebased so
 * it is relative to that path.
 *
 * Produced by {@link authorize}. `**` grants the path itself and everything beneath
 * it; the empty pattern grants exactly the path.
 */
export interface Permissions {
	/** Patterns the holder may subscribe to, relative to the authorized path. */
	subscribe: Patterns;
	/** Patterns the holder may publish to, relative to the authorized path. */
	publish: Patterns;
}

/**
 * The access `claims` grants at `path`, rebased so each returned pattern is relative
 * to `path`.
 *
 * `path` and `claims.root` must overlap, in either direction:
 *
 * - `path` extends the root (root `demo`, path `demo/room`), so the extra `room`
 *   narrows each pattern and drops the ones outside it.
 * - `path` is a parent of the root (root `demo`, path ``), so `demo` is prepended to
 *   each pattern to keep it anchored where the token points.
 *
 * Matching is segment-aware, so a root of `foo` does not cover `foobar`. Slashes at
 * the boundaries are implicit: `/demo/` and `demo` are the same path.
 *
 * Throws when the two don't overlap, and when they do but every pattern falls outside
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
	const beyondRoot = Path.stripPrefix(target, root);
	const beyondPath = Path.stripPrefix(root, target);

	let scope: (granted: Patterns) => Patterns;
	if (beyondRoot !== undefined) {
		// The path reaches into the grant; keep what each pattern says below it.
		scope = (granted) => granted.rebase(beyondRoot);
	} else if (beyondPath !== undefined) {
		// The grant sits below the path; name it from there.
		scope = (granted) => granted.rooted(beyondPath);
	} else {
		throw new Error(`path "${target}" does not overlap the token root "${root}"`);
	}

	const permissions: Permissions = {
		subscribe: scope(patterns(claims.subscribe)),
		publish: scope(patterns(claims.publish)),
	};
	if (permissions.subscribe.size === 0 && permissions.publish.size === 0) {
		throw new Error(`token grants no access to path "${target}"`);
	}

	return permissions;
}

/**
 * Whether every pattern `claims` grants is covered by `scope`, per role.
 *
 * Both sides are placed beneath their own root before comparing, so the same grant
 * expressed as `root: "demo"` + `publish: ["room/**"]` or as `publish: ["demo/room/**"]`
 * is treated identically. Containment is per pattern, so a scope of `live/**` does
 * not cover `lively/**`, and the roles are checked independently: a publish-only
 * scope never authorizes a subscribe grant.
 *
 * Must stay in lockstep with `Scope::allows` in the Rust `moq-auth` crate, which
 * checks the same keys.
 */
export function scopeAllows(scope: Scope, claims: Claims): boolean {
	const covers = (granted: string[] | undefined, requested: string[] | undefined) => {
		try {
			return patterns(granted).rooted(scope.root).covers(patterns(requested).rooted(claims.root));
		} catch {
			// A root too deep to place the patterns beneath cannot be granted either way.
			return false;
		}
	};

	return covers(scope.publish, claims.publish) && covers(scope.subscribe, claims.subscribe);
}
