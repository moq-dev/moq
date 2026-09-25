/**
 * The JSON encoding of the grants in claims and key scopes.
 *
 * Grants were once prefix lists named `put` and `get`, and every published `moq-token`
 * reader still expects them. Must stay in lockstep with `wire.rs` in the Rust
 * `moq-auth` crate.
 *
 * @module
 */

import { Pattern, Patterns } from "@moq/pattern";

/** One grant in whichever encoding it arrived: legacy `put`/`get` prefix lists, or patterns. */
type WireGrants = {
	put?: string | string[];
	get?: string | string[];
	publish?: string[];
	subscribe?: string[];
};

/**
 * Read legacy `moq-token` prefix grants as the subtree patterns they always meant.
 *
 * A prefix `p` is exactly `p/**` (and `""` is `**`). A document never mixes the two
 * encodings, and a legacy prefix containing `*` is refused: it had no wildcards, so
 * reading one now would silently widen the grant.
 */
export function decodeGrants<T extends WireGrants>(
	wire: T,
	ctx: { issues: unknown[] },
): Omit<T, "put" | "get"> & { publish?: string[]; subscribe?: string[] } {
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
 * Write grants the legacy way when every pattern is a subtree, so every published
 * `moq-token` reader agrees on what they grant. Anything a prefix can't say is written
 * as `publish`/`subscribe`, which an older reader refuses rather than misreads.
 */
export function encodeGrants<T extends { publish?: string[]; subscribe?: string[] }>(
	grants: T,
): Omit<T, "publish" | "subscribe"> & WireGrants {
	const { publish, subscribe, ...rest } = grants;
	const prefixes = (texts: string[] | undefined) => {
		const out: string[] = [];
		for (const pattern of new Patterns((texts ?? []).map((text) => Pattern.parse(text)))) {
			const prefix = pattern.asPrefix();
			if (prefix === undefined) return undefined;
			out.push(prefix);
		}
		return out;
	};

	const put = prefixes(publish);
	const get = prefixes(subscribe);
	if (put === undefined || get === undefined) {
		return {
			...rest,
			...(publish?.length && { publish }),
			...(subscribe?.length && { subscribe }),
		};
	}
	return { ...rest, ...(put.length && { put }), ...(get.length && { get }) };
}
