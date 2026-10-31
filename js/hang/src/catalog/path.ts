import { Epoch, Path } from "@moq/net";
import * as z from "@zod/mini";

function isRelative(text: string): boolean {
	try {
		Path.normalizeRelative(text);
		return true;
	} catch {
		return false;
	}
}

/**
 * Zod schema for a relative broadcast reference stored in a catalog (a rendition's
 * `broadcast` field, e.g. "./source"). Refuses and normalizes the input the same way the Rust
 * `path::Relative` type does so JS and Rust agree byte-for-byte after deserialization: a
 * leading, trailing, or doubled `/` or a lone `\` refuses the catalog.
 * Resolve it against the catalog broadcast's own path with `Path.tryResolve`, which returns
 * `undefined` for a reference that walks above the root: hang requires rejecting such a catalog.
 */
export const RelativeBroadcastSchema: z.ZodMiniType<RelativeBroadcast, string> = z.pipe(
	z.string().check(z.refine(isRelative, "expected a relative broadcast path")),
	z.transform(Path.normalizeRelative),
);

/**
 * A normalized relative broadcast reference: the same brand as `Path.Relative`, spelled
 * out here so the catalog schemas' inferred types stay nameable from this package.
 */
export type RelativeBroadcast = string & { __brand: "Relative" };

function isEpoch(text: string): boolean {
	try {
		Epoch.parse(text);
		return true;
	} catch {
		return false;
	}
}

/**
 * Zod schema for the publisher instance a catalog reference names: an entry's `epoch` beside its
 * `broadcast`. A lowercase hyphenated UUIDv7, the same text MoQ routes carry; anything else
 * refuses the catalog.
 */
export const EpochSchema: z.ZodMiniType<ReferenceEpoch, string> = z.pipe(
	z.string().check(z.refine(isEpoch, "expected a lowercase hyphenated UUIDv7")),
	z.transform((text) => text as ReferenceEpoch),
);

/**
 * A validated reference epoch: the same brand as `Epoch.Valid` from `@moq/net`, spelled out here
 * so the catalog schemas' inferred types stay nameable from this package.
 */
export type ReferenceEpoch = string & { readonly _brand: "epoch" };
