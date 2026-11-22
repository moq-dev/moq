import * as z from "zod/mini";
import { u53Schema } from "./catalog/integers.ts";

/** An optional measurement, with JSON null decoded as unavailable like Rust Option. */
export const measurementSchema = z.optional(
	z.pipe(
		z.nullable(u53Schema),
		z.transform((value) => value ?? undefined),
	),
);

/**
 * A report's per-rendition map, keyed by the catalog's rendition IDs: any string, `__proto__`
 * included, with every entry parsed by `schema`. Unlike a catalog map, an entry carries no
 * `version`, so none is skipped. Mirrors the Rust `BTreeMap<String, Rendition>`.
 */
export function renditionsSchema<T extends z.ZodMiniType>(schema: T) {
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
				else ctx.issues.push({ code: "custom", message: `malformed rendition ${id}: ${result.error.message}`, input: value });
			}
			// fromEntries defines each key as its own property, so `__proto__` stays an ID.
			return Object.fromEntries(kept) as Record<string, z.infer<T>>;
		}),
	);
}
