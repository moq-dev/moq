/**
 * The JSON encoding of the grants in claims and key scopes.
 *
 * Grants were once prefix lists named `put` and `get`, and every published `moq-token`
 * reader still expects them. Must stay in lockstep with `wire.rs` in the Rust
 * `moq-auth` crate.
 *
 * @module
 */

import { Pattern } from "@moq/pattern";
import type * as z from "@zod/mini";
import { type AccessSchema, normalizeAccess } from "./access.ts";

type AccessInput = z.input<typeof AccessSchema>;

/** One grant in whichever encoding it arrived: legacy `put`/`get` prefix lists, or patterns. */
type WireGrants = {
	put?: string | string[];
	get?: string | string[];
	publish?: AccessInput;
	subscribe?: AccessInput;
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
): Omit<T, "put" | "get" | "publish" | "subscribe"> & { publish?: AccessInput; subscribe?: AccessInput } {
	const { put, get, publish, subscribe, ...rest } = wire;
	const modern = {
		...rest,
		...(publish !== undefined && { publish }),
		...(subscribe !== undefined && { subscribe }),
	};
	if (put === undefined && get === undefined) return modern;

	const fail = (message: string) => {
		ctx.issues.push({ code: "custom", message, input: wire });
		return modern;
	};
	if (publish !== undefined || subscribe !== undefined) {
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
export function encodeGrants<T extends { publish?: AccessInput; subscribe?: AccessInput }>(
	grants: T,
): Omit<T, "publish" | "subscribe"> & WireGrants {
	const { publish, subscribe, ...rest } = grants;
	const pub = publish === undefined ? undefined : normalizeAccess(publish);
	const sub = subscribe === undefined ? undefined : normalizeAccess(subscribe);
	const prefixes = (access: typeof pub) => {
		if (access?.rate !== undefined) return undefined;
		const out: string[] = [];
		for (const pattern of access?.path ?? []) {
			const prefix = pattern.asPrefix();
			if (prefix === undefined) return undefined;
			out.push(prefix);
		}
		return out;
	};
	const put = prefixes(pub);
	const get = prefixes(sub);
	if (put === undefined || get === undefined) {
		return {
			...rest,
			...((pub?.path.size || pub?.rate !== undefined) && { publish: pub.toJSON() }),
			...((sub?.path.size || sub?.rate !== undefined) && { subscribe: sub.toJSON() }),
		};
	}
	return { ...rest, ...(put.length && { put }), ...(get.length && { get }) };
}
