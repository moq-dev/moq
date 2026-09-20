import { Path } from "@moq/net";
import * as z from "@zod/mini";

/**
 * Zod schema for a relative broadcast reference stored in a catalog (a rendition's
 * `broadcast` field, e.g. "./source"). Normalizes the input the same way the Rust
 * `path::Relative` type does so JS and Rust agree byte-for-byte after deserialization.
 * Resolve it against the catalog broadcast's own path with `Path.tryResolve`, which returns
 * `undefined` for a reference that walks above the root: hang requires rejecting such a catalog.
 */
export const RelativeBroadcastSchema: z.ZodMiniType<RelativeBroadcast, string> = z.pipe(
	z.string(),
	z.transform(Path.normalizeRelative),
);

/**
 * A normalized relative broadcast reference: the same brand as `Path.Relative`, spelled
 * out here so the catalog schemas' inferred types stay nameable from this package.
 */
export type RelativeBroadcast = string & { __brand: "Relative" };
