import { Path } from "@moq/net";
import * as z from "zod/mini";

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
 * leading, trailing, or doubled `/`, a lone `\`, or a lone surrogate refuses the catalog.
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

const utf8 = new TextEncoder();

/**
 * Zod schema for the stream a catalog reference names: an entry's `identity` beside its
 * `broadcast`. Any non-empty text of at most 256 UTF-8 bytes, the same text MoQ routes carry,
 * naming the publisher's stream; an empty, longer, or ill-formed one (a lone surrogate) refuses
 * the catalog.
 */
export const IdentitySchema: z.ZodMiniType<ReferenceIdentity, string> = z.pipe(
	z.string().check(
		z.minLength(1, "expected a non-empty identity"),
		z.refine((text) => text.isWellFormed(), "expected well-formed identity text"),
		z.refine((text) => utf8.encode(text).byteLength <= 256, "expected an identity of at most 256 UTF-8 bytes"),
	),
	z.transform((text) => text as ReferenceIdentity),
);

/**
 * A validated reference identity: the same brand as `Identity.Valid` from `@moq/net`, spelled
 * out here so the catalog schemas' inferred types stay nameable from this package.
 */
export type ReferenceIdentity = string & { readonly _brand: "identity" };
