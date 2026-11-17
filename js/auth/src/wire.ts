/**
 * The JSON encoding of the grants in claims, key scopes, and auth server grants.
 *
 * Grants were once prefix lists named `put` and `get`, and every published `moq-token`
 * reader still expects them. A role with a bitrate cap is written as an object,
 * `{"path": [...], "rate": bps}`, which every older reader refuses rather than drop the
 * cap, so a capped role is never written as prefixes. Must stay in lockstep with
 * `wire.rs` in the Rust `moq-auth` crate.
 *
 * @module
 */

import { Pattern, Patterns } from "@moq/pattern";
import * as z from "zod/mini";

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
 * What one role (publish or subscribe) reaches: a list of patterns, or exactly `path` and
 * `rate` (bits per second) when capped, so a misspelled cap is refused rather than read as
 * none. Every older reader refuses the capped form rather than drop the cap. A key scope
 * bounds paths, never bitrate, so it takes only the list.
 *
 * Mirrors `moq_auth::Access` in the Rust crate.
 */
export const AccessSchema = z.union([
	PatternListSchema,
	z.strictObject({
		/** Patterns the role reaches, relative to the root. */
		path: PatternListSchema,
		/** The most the role may move, in bits per second. */
		rate: RateSchema,
	}),
]);
/** A value parsed by {@link AccessSchema}. */
export type Access = z.output<typeof AccessSchema>;

/** The patterns `access` reaches, empty when the role is absent. */
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
 * Read legacy `moq-token` prefix grants as the subtree patterns they always meant.
 *
 * A prefix `p` is exactly `p/**` (and `""` is `**`). A document never mixes the two
 * encodings, and a legacy prefix containing `*` is refused: it had no wildcards, so
 * reading one now would silently widen the grant.
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

	const subtrees = (prefixes: string | string[] | undefined) =>
		prefixes === undefined
			? undefined
			: (typeof prefixes === "string" ? [prefixes] : prefixes).map((prefix) => Pattern.subtree(prefix).text);
	try {
		return { ...rest, publish: subtrees(put), subscribe: subtrees(get) };
	} catch (error) {
		return fail(`legacy prefix: ${error instanceof Error ? error.message : String(error)}`);
	}
}

/**
 * Write grants the legacy way when every pattern is a subtree and no role is capped, so
 * every published `moq-token` reader agrees on what they grant. Anything a prefix can't
 * say is written as `publish`/`subscribe`, which an older reader refuses rather than
 * misreads. An empty uncapped role is omitted either way.
 */
export function encodeGrants<T extends { publish?: Access; subscribe?: Access }>(
	grants: T,
): Omit<T, "publish" | "subscribe"> & WireGrants {
	const { publish, subscribe, ...rest } = grants;
	const prefixes = (access: Access | undefined) => {
		// A cap has no prefix form, so one moves the whole document to patterns.
		if (access !== undefined && !Array.isArray(access)) return undefined;
		const out: string[] = [];
		for (const pattern of new Patterns(paths(access).map((text) => Pattern.parse(text)))) {
			const prefix = pattern.asPrefix();
			if (prefix === undefined) return undefined;
			out.push(prefix);
		}
		return out;
	};
	const put = prefixes(publish);
	const get = prefixes(subscribe);
	if (put === undefined || get === undefined) {
		const kept = (access: Access | undefined) =>
			access !== undefined && (!Array.isArray(access) || access.length > 0) ? access : undefined;
		const pub = kept(publish);
		const sub = kept(subscribe);
		return { ...rest, ...(pub && { publish: pub }), ...(sub && { subscribe: sub }) };
	}
	return { ...rest, ...(put.length && { put }), ...(get.length && { get }) };
}
