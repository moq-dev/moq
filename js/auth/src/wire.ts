/**
 * The JSON encoding of claims, key scopes, and auth server grants.
 *
 * Grants are written as `publish` and `subscribe` prefix lists. They were once named
 * `put` and `get`, which are still read as the same prefixes. A role with a bitrate cap is
 * written as an object, `{"path": [...], "rate": bps}`, which every older reader refuses
 * rather than drop the cap. Until the version migrations {@link readPrefix} names, a prefix
 * is written and read as the subtree pattern releases before 0.22 spell it. Must stay in
 * lockstep with `wire.rs` in the Rust `moq-auth` crate.
 *
 * @module
 */

import { Prefix, parts } from "@moq/net/prefix";
import * as z from "zod/mini";
import type { Claims } from "./claims.ts";

// The most segments a path may have; a prefix this deep has nothing beneath it.
const MAX_SEGMENTS = 32;

/**
 * A list of prefix texts, each validated by `Prefix.parse`. A member holding a `*` is refused
 * with the prefix that replaces it, since a grant takes no wildcard.
 */
export const PrefixListSchema = z.array(
	z.string().check(
		z.superRefine((text, ctx) => {
			try {
				Prefix.parse(text);
			} catch (error) {
				ctx.addIssue({ code: "custom", message: error instanceof Error ? error.message : String(error) });
			}
		}),
	),
);

/**
 * TEMPORARY MIGRATION: the prefix a grant member names, as releases before 0.22 spell it.
 * Mirrors `alias` in the Rust crate's `wire.rs`.
 *
 * Those releases read a member as a pattern: a bare `foo` is the one path `foo`, and `""` the
 * root alone. So every list one of them reads, in a token, a key scope, or an auth server's
 * grant, is written as the same subtrees in pattern form ({@link written}), and a member
 * spelled that way, `foo/**` or `**`, reads back as its prefix. Any other wildcard is still
 * refused, and nothing else takes the alias: `Key.sign`'s claims and the CLI's flags stay strict.
 *
 * Deleted in two steps (`quest/m4/grant-alias-delete.md`): the first version migration after
 * 0.22 writes bare prefixes, and the one after it deletes the read, once tokens and key scopes
 * minted with `/**` have expired or been re-minted.
 */
function readPrefix(text: string): Prefix {
	const head = text === "**" ? "" : text.length > 3 && text.endsWith("/**") ? text.slice(0, -3) : text;
	// A wildcard left in the head names something other than one subtree; parsing all of it says so.
	return Prefix.parse(head.includes("*") ? text : head);
}

/** TEMPORARY MIGRATION (see {@link readPrefix}): the prefixes a list names, in either spelling. */
export function readPrefixes(texts: readonly string[] | undefined): string[] {
	return (texts ?? []).map((text) => readPrefix(text).text);
}

/**
 * TEMPORARY MIGRATION (see {@link readPrefix}): each prefix spelled as the subtree pattern a release
 * before 0.22 reads, `foo/**` or `**`, from either spelling. A prefix as deep as a path may be is its
 * literal, which a release reads as that one path: nothing lies beneath it.
 */
function written(texts: readonly string[]): string[] {
	return texts.map((text) => {
		const prefix = readPrefix(text);
		if (prefix.isEmpty) return "**";
		return prefix.segments.length === MAX_SEGMENTS ? prefix.text : `${prefix.text}/**`;
	});
}

/**
 * TEMPORARY MIGRATION (see {@link readPrefix}): a prefix list as a token, key scope, or grant
 * spells it, read as its prefixes.
 */
export const WirePrefixListSchema = z.array(
	z.pipe(
		z.string(),
		z.transform((text, ctx) => {
			try {
				return readPrefix(text).text;
			} catch (error) {
				ctx.issues.push({
					code: "custom",
					message: error instanceof Error ? error.message : String(error),
					input: text,
				});
				return text;
			}
		}),
	),
);

/**
 * A time in whole unix seconds, from the epoch to the last second the Rust crate's clock
 * holds. Anything else is refused, `null` included, so both languages read the same times.
 */
export const UnixSecondsSchema = z.int().check(z.gte(0), z.lte(18_446_744_073));

// At most 2^53 - 1, so every JSON reader holds the cap exactly.
const RateSchema = z.number().check(
	z.refine((bps) => Number.isSafeInteger(bps) && bps > 0, {
		message: "rate must be a positive integer of at most 9007199254740991 bits per second",
	}),
);

/**
 * What one role (publish or subscribe) reaches: a list of prefixes, or exactly `path` and
 * `rate` (bits per second) when capped, so a misspelled cap is refused rather than read as
 * none. Every older reader refuses the capped form rather than drop the cap. A key scope
 * bounds paths, never bitrate, so it takes only the list.
 *
 * Mirrors `moq_auth::Access` in the Rust crate.
 */
export const AccessSchema = z.union([
	PrefixListSchema,
	z.strictObject({
		/** Prefixes the role reaches, relative to the root, each covering its subtree. */
		path: PrefixListSchema,
		/** The most the role may move, in bits per second. */
		rate: RateSchema,
	}),
]);
/** A value parsed by {@link AccessSchema}. */
export type Access = z.output<typeof AccessSchema>;

/** TEMPORARY MIGRATION (see {@link readPrefix}): a role as a token or grant spells it, read as its prefixes. */
export const WireAccessSchema = z.union([
	WirePrefixListSchema,
	z.strictObject({ path: WirePrefixListSchema, rate: RateSchema }),
]);

/** TEMPORARY MIGRATION (see {@link readPrefix}): `access` with each prefix spelled as {@link written} does. */
export function writtenAccess(access: Access): Access {
	return Array.isArray(access) ? written(access) : { ...access, path: written(access.path) };
}

/**
 * TEMPORARY MIGRATION (see {@link readPrefix}): a key scope with each prefix spelled as
 * {@link written} does, so a key's JWK reads as the same subtrees as the tokens it signs.
 */
export function writtenScope<T extends { publish?: string[]; subscribe?: string[] }>(scope: T): T {
	const { publish, subscribe } = scope;
	return {
		...scope,
		...(publish && { publish: written(publish) }),
		...(subscribe && { subscribe: written(subscribe) }),
	};
}

/** The prefixes `access` reaches, empty when the role is absent. */
export function paths(access: Access | undefined): string[] {
	if (access === undefined) return [];
	return Array.isArray(access) ? access : access.path;
}

/** One grant in whichever encoding it arrived: legacy `put`/`get` prefix lists, or roles. */
type WireGrants = {
	put?: string | string[];
	get?: string | string[];
	publish?: Access;
	subscribe?: Access;
};

/**
 * Read legacy `moq-token` prefix grants as the prefixes they always were.
 *
 * Their slashes were implicit at the boundaries, so `/room/` reads as `room`. A document never
 * mixes the two encodings, and a legacy prefix containing `*` is refused: it had no wildcards,
 * so reading one now would hand out a grant nobody wrote.
 */
export function decodeGrants<T extends WireGrants>(wire: T, ctx: { issues: unknown[] }): Omit<T, "put" | "get"> {
	const { put, get, ...rest } = wire;
	if (put === undefined && get === undefined) return rest;

	const fail = (message: string) => {
		ctx.issues.push({ code: "custom", message, input: wire });
		return rest;
	};
	if (rest.publish !== undefined || rest.subscribe !== undefined) {
		return fail("mixes the legacy put/get fields with publish/subscribe");
	}

	const prefixes = (texts: string | string[] | undefined) =>
		texts === undefined
			? undefined
			: (typeof texts === "string" ? [texts] : texts).map(
					(text) =>
						Prefix.parse(
							parts(text)
								.filter((part) => part !== "")
								.join("/"),
						).text,
				);
	try {
		return { ...rest, publish: prefixes(put), subscribe: prefixes(get) };
	} catch (error) {
		return fail(`legacy prefix: ${error instanceof Error ? error.message : String(error)}`);
	}
}

/**
 * Omit a role that grants nothing and carries no cap, as both libraries write it, and spell the
 * rest as {@link writtenAccess} does.
 */
export function encodeGrants<T extends { publish?: Access; subscribe?: Access }>(
	grants: T,
): Omit<T, "publish" | "subscribe"> & { publish?: Access; subscribe?: Access } {
	const { publish, subscribe, ...rest } = grants;
	const kept = (access: Access | undefined) =>
		access !== undefined && (!Array.isArray(access) || access.length > 0) ? writtenAccess(access) : undefined;
	const pub = kept(publish);
	const sub = kept(subscribe);
	return { ...rest, ...(pub && { publish: pub }), ...(sub && { subscribe: sub }) };
}

/**
 * A token's identifier, its `jti` claim, such as a UUID an issuer records to revoke it.
 * One to 128 printable ASCII characters, so it logs safely and both libraries bound it by
 * the same length. An issuer picks it; nothing here invents one.
 *
 * Mirrors `moq_auth::TokenId` in the Rust crate.
 */
export const TokenIdSchema = z
	.string()
	.check(
		z.minLength(1),
		z.maxLength(128),
		z.regex(/^[\x21-\x7e]*$/, { message: "Token ID must be printable ASCII without spaces" }),
	);

/** Write claims as a token's JSON payload. */
export function encodeClaims({ expires, issued, notBefore, id, ...grants }: Claims) {
	return {
		...encodeGrants(grants),
		...(expires !== undefined && { exp: expires }),
		...(issued !== undefined && { iat: issued }),
		...(notBefore !== undefined && { nbf: notBefore }),
		...(id !== undefined && { jti: id }),
	};
}
