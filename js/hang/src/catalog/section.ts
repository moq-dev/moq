import * as z from "zod/mini";

/**
 * Wrap a catalog section so a value that isn't one decodes as absent rather than failing the
 * whole catalog.
 *
 * A section name is only reserved from the version that defines it, so an application may already
 * be carrying its own key under that name. Dropping the one key we can't read keeps the rest of
 * the catalog readable, instead of taking video and audio down with it.
 *
 * The fallback is deliberately narrow: only a value whose `map` key is missing or not an object is
 * treated as someone else's. A value that *is* a section but carries a malformed entry still fails,
 * since swallowing that would hide a publisher bug (a data track with no `mode`) behind the silence
 * that exists to protect an unrelated key. Mirrors the Rust `deserialize_section`, which recognizes
 * the section only when the map is an object.
 *
 * @param schema - the section's own schema, applied when the value looks like a section.
 * @param map - the key holding the section's map of tracks (`tracks`, `renditions`).
 */
export function section<T extends z.ZodMiniType>(schema: T, map: string) {
	return z.optional(
		z.union([
			schema,
			z.pipe(
				z.custom<unknown>((value) => {
					if (typeof value !== "object" || value === null || !(map in value)) return true;
					const inner = (value as Record<string, unknown>)[map];
					return typeof inner !== "object" || inner === null || Array.isArray(inner);
				}),
				z.transform(() => undefined),
			),
		]),
	);
}
