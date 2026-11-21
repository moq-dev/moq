/**
 * The payload of a token: a root, plus the publish/subscribe prefixes granted beneath it
 * and an optional bitrate cap on each.
 *
 * @module
 */

import { Prefixes } from "@moq/pattern";
import * as z from "zod/mini";
import * as Path from "./path.ts";
import {
	AccessSchema,
	decodeGrants,
	paths,
	readPrefixes,
	TokenIdSchema,
	UnixSecondsSchema,
	WireAccessSchema,
	WirePrefixListSchema,
	writtenScope,
} from "./wire.ts";

export { type Access, AccessSchema, PrefixListSchema, TokenIdSchema } from "./wire.ts";

/** A value parsed by {@link TokenIdSchema}. */
export type TokenId = z.output<typeof TokenIdSchema>;

/** Parse a list of prefix texts into a reduced set; a wildcard throws `InvalidPrefix`. */
export function prefixes(texts: readonly string[] | undefined): Prefixes {
	return Prefixes.parse(texts ?? []);
}

/**
 * The immutable ceiling on what a key may grant, embedded in its JWK.
 *
 * `root` is optional on the wire to match the Rust `moq-auth` crate, which omits it
 * when the scope sits at the top level. A legacy `put`/`get` scope reads as the same
 * prefixes, and so does a member spelled as a subtree pattern, `foo/**` or `**`, as released
 * signers wrote it. A scope bounds paths, never bitrate, so a capped role is
 * refused, as is a malformed root (see `Path.validate`). The stricter `Path.signable` applies
 * only where a scope is minted, so keys minted before it still load. Any other field is refused.
 *
 * TEMPORARY MIGRATION (see `wire.ts`): the parsed scope spells each prefix as the subtree pattern
 * a release before 0.22 reads, `foo/**` or `**`, as `GrantSchema` does. So a parsed key, and
 * `Key.public` and `KeySet.public` of any key, write a scope every verifier reads as the same
 * subtrees as the tokens the key signs. {@link Scope.allows} reads either spelling.
 */
export const ScopeSchema = z.pipe(
	z
		.pipe(
			z.strictObject({
				/** The root that `publish` and `subscribe` are relative to. Defaults to the empty string. */
				root: z._default(z.string(), ""),
				/** Prefixes this key may grant to publishers, relative to `root`. */
				publish: z.optional(WirePrefixListSchema),
				/** Prefixes this key may grant to subscribers, relative to `root`. */
				subscribe: z.optional(WirePrefixListSchema),
				put: z.optional(z.array(z.string())),
				get: z.optional(z.array(z.string())),
			}),
			z.transform(decodeGrants),
		)
		.check(
			z.refine((data) => (data.publish?.length ?? 0) > 0 || (data.subscribe?.length ?? 0) > 0, {
				message: "Either publish or subscribe must contain at least one prefix",
			}),
			z.superRefine((data, ctx) => {
				try {
					Path.validate(data.root);
				} catch (error) {
					ctx.addIssue({ code: "custom", message: error instanceof Error ? error.message : String(error) });
				}
			}),
		),
	z.transform(writtenScope),
);

/** A value parsed by {@link ScopeSchema}. */
export type Scope = z.output<typeof ScopeSchema>;

/**
 * {@link Claims} as this library names them, the one definition of their fields. The wire
 * spells the times and ID as the registered `exp`, `iat`, `nbf`, and `jti`, which
 * {@link ClaimsSchema} reads. {@link Key.sign} parses with it, so a misspelled field,
 * such as the wire's `exp` or a legacy `put`, is refused.
 */
export const ClaimsFieldsSchema = z
	.strictObject({
		/** The root that `publish` and `subscribe` are relative to. Defaults to the empty string. */
		root: z._default(z.string(), ""),
		/** What the holder may publish, relative to `root`, optionally with a bitrate cap. */
		publish: z.optional(AccessSchema),
		/** What the holder may subscribe to, relative to `root`, optionally with a bitrate cap. */
		subscribe: z.optional(AccessSchema),
		/** When the token expires (`exp`), as a whole unix timestamp in seconds. */
		expires: z.optional(UnixSecondsSchema),
		/** When the token was issued (`iat`), as a whole unix timestamp in seconds. Informational. */
		issued: z.optional(UnixSecondsSchema),
		/** The time before which the token is refused (`nbf`), as a whole unix timestamp in seconds. */
		notBefore: z.optional(UnixSecondsSchema),
		/** The token's identifier (`jti`), which its issuer may record to revoke it. */
		id: z.optional(TokenIdSchema),
	})
	.check(
		// Emptiness, not just presence: `publish: []` grants nothing, and the Rust crate
		// rejects such a token as useless. Checking `!== undefined` here would mint
		// tokens that Rust then refuses to verify.
		z.refine((data) => paths(data.publish).length > 0 || paths(data.subscribe).length > 0, {
			message: "Either publish or subscribe must grant at least one prefix",
		}),
	);

/**
 * The payload of a token, as {@link Key.sign} takes it and {@link Key.verify} returns it.
 *
 * A prefix covers itself and everything beneath it: `alice` is `alice` and its subtree, and
 * the empty prefix `""` is everything under the root. A member holding a `*` is refused,
 * at signing and at verification alike. A role may instead be an
 * {@link Access} with a cap, `{"path": [...], "rate": bps}`. The times and ID are written
 * as the registered `exp`, `iat`, `nbf`, and `jti` claims.
 */
export type Claims = z.output<typeof ClaimsFieldsSchema>;

// Registered claims that narrow nothing: an issuer's bookkeeping, read and dropped.
// Every other claim is refused, since an unknown one may narrow the grant, and a
// misspelled `root` would otherwise widen it to everything.
const IgnoredFields = {
	iss: z.optional(z.unknown()),
	sub: z.optional(z.unknown()),
};

// Legacy `put`/`get` took one prefix or a list.
const LegacyPrefixListSchema = z.union([z.string(), z.array(z.string())]);

/**
 * A token's JSON payload, parsed into {@link Claims}.
 *
 * `root` is optional on the wire: a token scoped to the top-level path omits it, so
 * it defaults to the empty string to match the Rust `moq-auth` crate. Legacy `moq-token`
 * claims read too, each `put`/`get` prefix as the same prefix; signing always
 * writes `publish` and `subscribe`, each prefix spelled as the subtree pattern a release before
 * 0.22 reads (`foo/**`, `**`), and a member spelled that way reads back as its prefix. Any other
 * wildcard fails verification. `exp`, `iat`, `nbf`, and `jti` read as `expires`,
 * `issued`, `notBefore`, and `id`. The registered `iss` and `sub` claims are read and
 * dropped; any other field fails verification.
 */
export const ClaimsSchema = z.pipe(
	z.pipe(
		z.strictObject({
			root: z.optional(z.string()),
			publish: z.optional(WireAccessSchema),
			subscribe: z.optional(WireAccessSchema),
			exp: z.optional(UnixSecondsSchema),
			iat: z.optional(UnixSecondsSchema),
			nbf: z.optional(UnixSecondsSchema),
			jti: z.optional(TokenIdSchema),
			...IgnoredFields,
			put: z.optional(LegacyPrefixListSchema),
			get: z.optional(LegacyPrefixListSchema),
		}),
		// Fed to the strict fields, so a claim misnamed here is refused rather than dropped.
		z.transform(
			({ iss: _iss, sub: _sub, exp, iat, nbf, jti, ...grants }, ctx): z.input<typeof ClaimsFieldsSchema> => ({
				...decodeGrants(grants, ctx),
				...(exp !== undefined && { expires: exp }),
				...(iat !== undefined && { issued: iat }),
				...(nbf !== undefined && { notBefore: nbf }),
				...(jti !== undefined && { id: jti }),
			}),
		),
	),
	ClaimsFieldsSchema,
);

/**
 * The access a {@link Claims} grants at a specific path, with every prefix rebased so
 * it is relative to that path.
 *
 * Produced by {@link Claims.authorize}. The empty prefix grants the path itself and
 * everything beneath it.
 */
export interface Permissions {
	/** Prefixes the holder may subscribe to, relative to the authorized path. */
	subscribe: Prefixes;
	/** Prefixes the holder may publish to, relative to the authorized path. */
	publish: Prefixes;
}

/**
 * The access `claims` grants at `path`, rebased so each returned prefix is relative
 * to `path`.
 *
 * `path` and `claims.root` must overlap, in either direction:
 *
 * - `path` extends the root (root `demo`, path `demo/room`), so the extra `room`
 *   narrows each prefix and drops the ones outside it.
 * - `path` is a parent of the root (root `demo`, empty path), so `demo` is prepended to
 *   each prefix to keep it anchored where the token points.
 *
 * Matching is segment-aware, so a root of `foo` does not cover `foobar`. Slashes at
 * the boundaries are implicit: `/demo/` and `demo` are the same path.
 *
 * Throws when the two don't overlap, and when they do but every prefix falls outside
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

	let scope: (granted: Prefixes) => Prefixes;
	if (beyondRoot !== undefined) {
		// The path reaches into the grant; keep what each prefix says below it.
		scope = (granted) => granted.rebase(beyondRoot);
	} else if (beyondPath !== undefined) {
		// The grant sits below the path; name it from there.
		scope = (granted) => granted.rooted(beyondPath);
	} else {
		throw new Error(`path "${target}" does not overlap the token root "${root}"`);
	}

	const permissions: Permissions = {
		subscribe: scope(prefixes(paths(claims.subscribe))),
		publish: scope(prefixes(paths(claims.publish))),
	};
	if (permissions.subscribe.size === 0 && permissions.publish.size === 0) {
		throw new Error(`token grants no access to path "${target}"`);
	}

	return permissions;
}

/**
 * Whether every prefix `claims` grants is covered by `scope`, per role.
 *
 * Both sides are placed beneath their own root before comparing, so the same grant
 * expressed as `root: "demo"` + `publish: ["room"]` or as `publish: ["demo/room"]`
 * is treated identically. Coverage is segment-aware, so a scope of `live` does not
 * cover `lively`, and the roles are checked independently: a publish-only scope never
 * authorizes a subscribe grant.
 *
 * Must stay in lockstep with `Scope::allows` in the Rust `moq-auth` crate, which
 * checks the same keys.
 */
function allows(scope: Scope, claims: Claims): boolean {
	const covers = (granted: string[] | undefined, requested: string[] | undefined) => {
		try {
			// TEMPORARY MIGRATION (see `wire.ts`): a scope spells its prefixes as subtree patterns.
			return prefixes(readPrefixes(granted)).rooted(scope.root).covers(prefixes(requested).rooted(claims.root));
		} catch {
			// A root too deep to place the prefixes beneath cannot be granted either way.
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
	/** The access `claims` grants at `path`, rebased so each returned prefix is relative to `path`. */
	authorize,
};

/**
 * Check claims against a key's scope.
 *
 * The counterpart of the Rust `moq-auth` crate's `Scope` methods.
 */
export const Scope = {
	/** Whether every prefix `claims` grants is covered by `scope`, per role. */
	allows,
};
