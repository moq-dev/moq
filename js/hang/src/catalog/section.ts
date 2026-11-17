import * as z from "zod/mini";
import { u53Schema } from "./integers";
import { VERSION, VersionSchema } from "./version";

/** The entries each parsed map skipped, keyed by the map itself. Read through `skipped` in `root.ts`. */
export const SKIPPED = new WeakMap<object, Record<string, number>>();

/**
 * A catalog map keyed by ID (renditions, data tracks, includes), skipping an entry newer than this
 * reader and refusing the catalog if any other entry is malformed.
 *
 * An entry's `version` is read before the rest of it, since a newer draft may have reshaped the
 * entry; one above {@link VERSION} is skipped and recorded in {@link SKIPPED}. Every other entry
 * must parse with `schema`; the issue names the entry. An unknown container or extension field is
 * not malformed, so it never refuses the catalog. Mirrors the Rust `deserialize_entries`.
 *
 * @param schema - one entry's schema.
 * @param kind - what an entry is, naming it in the issue (`rendition`, `include`).
 */
export function renditions<T extends z.ZodMiniType>(schema: T, kind = "rendition") {
	return z.pipe(
		// Not `z.record`, which would drop an ID like `__proto__` that JSON.parse keeps as its own.
		z.custom<Record<string, unknown>>(
			(value) => typeof value === "object" && value !== null && !Array.isArray(value),
		),
		z.transform((raw, ctx) => {
			const kept: [string, z.infer<T>][] = [];
			const skipped: [string, number][] = [];
			for (const [id, value] of Object.entries(raw)) {
				let entry = value;
				if (typeof value === "object" && value !== null && Object.hasOwn(value, "version")) {
					// Taken out rather than left to the entry: written versions are computed, never carried.
					const { version, ...rest } = value as Record<string, unknown>;
					const checked = VersionSchema.safeParse(version);
					if (!checked.success) {
						ctx.issues.push({
							code: "custom",
							message: `malformed ${kind} ${id}: ${checked.error.message}`,
							input: value,
						});
						continue;
					}
					if (checked.data > VERSION) {
						skipped.push([id, checked.data]);
						continue;
					}
					entry = rest;
				}

				const result = schema.safeParse(entry);
				if (result.success) kept.push([id, result.data]);
				else
					ctx.issues.push({
						code: "custom",
						message: `malformed ${kind} ${id}: ${result.error.message}`,
						input: value,
					});
			}
			// fromEntries defines each key as its own property, so `__proto__` stays an ID.
			const map = Object.fromEntries(kept) as Record<string, z.infer<T>>;
			if (skipped.length > 0) SKIPPED.set(map, Object.fromEntries(skipped));
			return map;
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
