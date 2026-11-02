import { Path } from "@moq/net";
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

/**
 * Zod schema for the stream a catalog reference names: an entry's `instance` beside its
 * `broadcast`. Any non-empty text, the same text MoQ routes carry; an empty one refuses the
 * catalog.
 */
export const InstanceSchema: z.ZodMiniType<ReferenceInstance, string> = z.pipe(
	z.string().check(z.minLength(1, "expected a non-empty instance")),
	z.transform((text) => text as ReferenceInstance),
);

/**
 * A validated reference instance: the same brand as `Instance.Valid` from `@moq/net`, spelled
 * out here so the catalog schemas' inferred types stay nameable from this package.
 */
export type ReferenceInstance = string & { readonly _brand: "instance" };
