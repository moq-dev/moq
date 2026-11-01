import * as z from "zod/mini";
import { u53Schema } from "./integers";

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

/**
 * A media section's renditions, refusing the catalog if any one is malformed.
 *
 * Each rendition must parse with `schema`; the issue names the rendition. An unknown container
 * or extension field is not malformed, so it never refuses the catalog. Mirrors the Rust
 * `deserialize_renditions`.
 *
 * @param schema - one rendition's schema.
 */
export function renditions<T extends z.ZodMiniType>(schema: T) {
	return z.pipe(
		// Not `z.record`, which would drop an ID like `__proto__` that JSON.parse keeps as its own.
		z.custom<Record<string, unknown>>(
			(value) => typeof value === "object" && value !== null && !Array.isArray(value),
		),
		z.transform((raw, ctx) => {
			const kept: [string, z.infer<T>][] = [];
			for (const [id, value] of Object.entries(raw)) {
				const result = schema.safeParse(value);
				if (result.success) kept.push([id, result.data]);
				else
					ctx.issues.push({
						code: "custom",
						message: `malformed rendition ${id}: ${result.error.message}`,
						input: value,
					});
			}
			// fromEntries defines each key as its own property, so `__proto__` stays an ID.
			return Object.fromEntries(kept) as Record<string, z.infer<T>>;
		}),
	);
}

/**
 * A nonzero count that fits in 32 bits, such as a picture dimension or sample rate. Branded `u53`
 * like the wider counts, since every value fits there too.
 */
export const countSchema = u53Schema.check(z.positive(), z.lte(0xffff_ffff));

/**
 * Whether a codec string field is digits in `radix`, at most `max`. Refuses the empty field and
 * the sign or whitespace `parseInt` alone would take. Mirrors the Rust `codec_number`.
 */
export function codecNumber(field: string | undefined, radix: 10 | 16, max: number): boolean {
	if (field === undefined) return false;
	const digits = radix === 10 ? /^[0-9]+$/ : /^[0-9a-fA-F]+$/;
	return digits.test(field) && Number.parseInt(field, radix) <= max;
}

/**
 * Like {@link section}, for a section identified by one string field (`stats.track`, `echo.path`)
 * rather than a map. Mirrors the Rust `deserialize_named`.
 *
 * @param schema - the section's own schema, applied when the field is a string.
 * @param key - the section's required string field.
 */
export function named<T extends z.ZodMiniType>(schema: T, key: string) {
	return z.optional(
		z.union([
			schema,
			z.pipe(
				z.custom<unknown>(
					(value) =>
						typeof value !== "object" ||
						value === null ||
						typeof (value as Record<string, unknown>)[key] !== "string",
				),
				z.transform(() => undefined),
			),
		]),
	);
}
