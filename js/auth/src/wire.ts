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
import * as z from "@zod/mini";

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

// At most 2^53 - 1, so every JSON reader holds the cap exactly.
const RateSchema = z.number().check(
	z.refine((bps) => Number.isSafeInteger(bps) && bps > 0, {
		message: "rate must be a positive integer of at most 9007199254740991 bits per second",
	}),
);

/**
 * One role in a claims or grant document: a list of patterns, or exactly `path` and
 * `rate`, so a misspelled cap is refused rather than read as none. A key scope bounds
 * paths, never bitrate, so it takes only the list.
 */
export const WireRoleSchema = z.union([
	PatternListSchema,
	z.strictObject({ path: PatternListSchema, rate: RateSchema }),
]);

/** One role as written. */
type WireRole = z.output<typeof WireRoleSchema>;

/** The decoded roles: pattern lists, each with an optional cap in bits per second. */
type Roles = { publish?: string[]; publish_rate?: number; subscribe?: string[]; subscribe_rate?: number };

/** One grant in whichever encoding it arrived: legacy `put`/`get` prefix lists, or roles. */
type WireGrants = {
	put?: string | string[];
	get?: string | string[];
	publish?: WireRole;
	subscribe?: WireRole;
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

/** Split each capped role into its patterns and a `_rate` field beside them. */
export function decodeRoles<T extends { publish?: WireRole; subscribe?: WireRole }>(
	wire: T,
): Omit<T, "publish" | "subscribe"> & {
	// Spelled out rather than `Roles`, so the public `Claims` and `Grant` types document each field.
	/** Patterns the holder may publish, relative to the root. */
	publish?: string[];
	/** The most the holder may publish, in bits per second, or no cap when absent. */
	publish_rate?: number;
	/** Patterns the holder may subscribe to, relative to the root. */
	subscribe?: string[];
	/** The most the holder may receive from its subscriptions, in bits per second, or no cap when absent. */
	subscribe_rate?: number;
} {
	const { publish, subscribe, ...rest } = wire;
	const split = (role: WireRole | undefined): { path?: string[]; rate?: number } =>
		role === undefined || Array.isArray(role) ? { path: role } : role;
	const pub = split(publish);
	const sub = split(subscribe);
	return {
		...(pub.path && { publish: pub.path }),
		...(pub.rate !== undefined && { publish_rate: pub.rate }),
		...(sub.path && { subscribe: sub.path }),
		...(sub.rate !== undefined && { subscribe_rate: sub.rate }),
		...rest,
	};
}

/**
 * Nest each `_rate` field back into its role, so the decoded claims `Key.verify`
 * returns parse again. A role already in the capped form is left for the schema to refuse.
 */
export function nestRates<
	T extends { publish?: unknown; subscribe?: unknown; publish_rate?: number; subscribe_rate?: number },
>(claims: T) {
	const { publish_rate, subscribe_rate, ...rest } = claims;
	return {
		...rest,
		...(publish_rate !== undefined && { publish: { path: rest.publish ?? [], rate: publish_rate } }),
		...(subscribe_rate !== undefined && { subscribe: { path: rest.subscribe ?? [], rate: subscribe_rate } }),
	};
}

/** Write each role as its patterns, or as the capped object when it has a rate; an empty uncapped role is omitted. */
export function encodeRoles(roles: Roles): { publish?: WireRole; subscribe?: WireRole } {
	const role = (path: string[] | undefined, rate: number | undefined): WireRole | undefined =>
		rate !== undefined ? { path: path ?? [], rate } : path?.length ? path : undefined;
	const publish = role(roles.publish, roles.publish_rate);
	const subscribe = role(roles.subscribe, roles.subscribe_rate);
	return { ...(publish && { publish }), ...(subscribe && { subscribe }) };
}

/**
 * Write grants the legacy way when every pattern is a subtree and no role is capped, so
 * every published `moq-token` reader agrees on what they grant. Anything a prefix can't
 * say is written as `publish`/`subscribe`, which an older reader refuses rather than misreads.
 */
export function encodeGrants<T extends Roles>(grants: T): Omit<T, keyof Roles> & WireGrants {
	const { publish, publish_rate, subscribe, subscribe_rate, ...rest } = grants;
	const prefixes = (texts: string[] | undefined, rate: number | undefined) => {
		// A cap has no prefix form, so one moves the whole document to patterns.
		if (rate !== undefined) return undefined;
		const out: string[] = [];
		for (const pattern of new Patterns((texts ?? []).map((text) => Pattern.parse(text)))) {
			const prefix = pattern.asPrefix();
			if (prefix === undefined) return undefined;
			out.push(prefix);
		}
		return out;
	};

	const put = prefixes(publish, publish_rate);
	const get = prefixes(subscribe, subscribe_rate);
	if (put === undefined || get === undefined) {
		return { ...rest, ...encodeRoles({ publish, publish_rate, subscribe, subscribe_rate }) };
	}
	return { ...rest, ...(put.length && { put }), ...(get.length && { get }) };
}
