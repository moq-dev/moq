/**
 * The payload of a token: a root, plus the publish/subscribe patterns granted beneath it
 * and an optional bitrate cap on each.
 *
 * @module
 */

import { Pattern, Patterns } from "@moq/pattern";
import * as z from "zod/mini";
import * as Path from "./path.ts";
import {
	type Access,
	AccessSchema,
	decodeGrants,
	PatternListSchema,
	paths,
	TokenIdSchema,
	UnixSecondsSchema,
} from "./wire.ts";

export { type Access, AccessSchema, PatternListSchema, TokenIdSchema } from "./wire.ts";

/** A value parsed by {@link TokenIdSchema}. */
export type TokenId = z.output<typeof TokenIdSchema>;

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
 * subtree patterns it meant. A scope bounds paths, never bitrate, so a capped role is
 * refused, as is a root `Key.sign` would refuse. Any other field is refused.
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
		z.superRefine((data, ctx) => {
			try {
				Path.validate(data.root);
			} catch (error) {
				ctx.addIssue({ code: "custom", message: error instanceof Error ? error.message : String(error) });
			}
		}),
	);

/** A value parsed by {@link ScopeSchema}. */
export type Scope = z.output<typeof ScopeSchema>;

/**
 * The payload of a token, as {@link Key.sign} takes it and {@link Key.verify} returns it.
 *
 * A pattern names exactly what it says: `alice` is one broadcast, `alice/**` is a
 * subtree, and `**` is everything under the root. A role may instead be an
 * {@link Access} with a cap, `{"path": [...], "rate": bps}`. The times and ID are written
 * as the registered `exp`, `iat`, `nbf`, and `jti` claims.
 */
export interface Claims {
	/** The root that `publish` and `subscribe` are relative to. */
	root: string;
	/** What the holder may publish, relative to `root`, optionally with a bitrate cap. */
	publish?: Access;
	/** What the holder may subscribe to, relative to `root`, optionally with a bitrate cap. */
	subscribe?: Access;
	/** When the token expires (`exp`), as a whole unix timestamp in seconds. */
	expires?: number;
	/** When the token was issued (`iat`), as a whole unix timestamp in seconds. Informational. */
	issued?: number;
	/** The time before which the token is refused (`nbf`), as a whole unix timestamp in seconds. */
	notBefore?: number;
	/** The token's identifier (`jti`), which its issuer may record to revoke it. */
	id?: TokenId;
}

// Registered claims that narrow nothing: an issuer's bookkeeping, read and dropped.
// Every other claim is refused, since an unknown one may narrow the grant, and a
// misspelled `root` would otherwise widen it to everything.
const IgnoredFields = {
	iss: z.optional(z.unknown()),
	sub: z.optional(z.unknown()),
};

const PrefixListSchema = z.union([z.string(), z.array(z.string())]);

/**
 * A token's JSON payload, parsed into {@link Claims}.
 *
 * `root` is optional on the wire: a token scoped to the top-level path omits it, so
 * it defaults to the empty string to match the Rust `moq-auth` crate. Legacy `moq-token`
 * claims read too, each `put`/`get` prefix `p` as the subtree `p/**`; signing always
 * writes `publish` and `subscribe`. `exp`, `iat`, `nbf`, and `jti` read as `expires`,
 * `issued`, `notBefore`, and `id`. The registered `iss` and `sub` claims are read and
 * dropped; any other field fails verification.
 */
export const ClaimsSchema = z
	.pipe(
		z.strictObject({
			root: z._default(z.string(), ""),
			publish: z.optional(AccessSchema),
			subscribe: z.optional(AccessSchema),
			exp: z.optional(UnixSecondsSchema),
			iat: z.optional(UnixSecondsSchema),
			nbf: z.optional(UnixSecondsSchema),
			jti: z.optional(TokenIdSchema),
			...IgnoredFields,
			put: z.optional(PrefixListSchema),
			get: z.optional(PrefixListSchema),
		}),
		z.transform(
			({ iss: _iss, sub: _sub, exp, iat, nbf, jti, ...grants }, ctx): Claims => ({
				...decodeGrants(grants, ctx),
				...(exp !== undefined && { expires: exp }),
				...(iat !== undefined && { issued: iat }),
				...(nbf !== undefined && { notBefore: nbf }),
				...(jti !== undefined && { id: jti }),
			}),
		),
	)
	.check(
		// Emptiness, not just presence: `publish: []` grants nothing, and the Rust crate
		// rejects such a token as useless. Checking `!== undefined` here would mint
		// tokens that Rust then refuses to verify.
		z.refine((data) => paths(data.publish).length > 0 || paths(data.subscribe).length > 0, {
			message: "Either publish or subscribe must grant at least one pattern",
		}),
	);

/**
 * The access a {@link Claims} grants at a specific path, with every pattern rebased so
 * it is relative to that path.
 *
 * Produced by {@link Claims.authorize}. `**` grants the path itself and everything beneath
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
 * - `path` is a parent of the root (root `demo`, empty path), so `demo` is prepended to
 *   each pattern to keep it anchored where the token points.
 *
 * Matching is segment-aware, so a root of `foo` does not cover `foobar`. Slashes at
 * the boundaries are implicit: `/demo/` and `demo` are the same path.
 *
 * Throws when the two don't overlap, and when they do but every pattern falls outside
 * `path`.
 *
 * This is authorization only. Verify the signature first with {@link Key.verify} or
 * {@link KeySet.verify}, which is where expiry is enforced.
 */
function authorize(claims: Claims, path: string): Permissions {
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
		subscribe: scope(patterns(paths(claims.subscribe))),
		publish: scope(patterns(paths(claims.publish))),
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
function allows(scope: Scope, claims: Claims): boolean {
	const covers = (granted: string[] | undefined, requested: string[] | undefined) => {
		try {
			return patterns(granted).rooted(scope.root).covers(patterns(requested).rooted(claims.root));
		} catch {
			// A root too deep to place the patterns beneath cannot be granted either way.
			return false;
		}
	};

	return covers(scope.publish, paths(claims.publish)) && covers(scope.subscribe, paths(claims.subscribe));
}

/**
 * Scope verified claims to a connection path.
 *
 * The counterpart of the Rust `moq-auth` crate's `Claims` methods.
 */
export const Claims = {
	/** The access `claims` grants at `path`, rebased so each returned pattern is relative to `path`. */
	authorize,
};

/**
 * Check claims against a key's scope.
 *
 * The counterpart of the Rust `moq-auth` crate's `Scope` methods.
 */
export const Scope = {
	/** Whether every pattern `claims` grants is covered by `scope`, per role. */
	allows,
};
