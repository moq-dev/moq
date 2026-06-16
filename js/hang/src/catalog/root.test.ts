import { expect, test } from "bun:test";
import * as z from "zod/mini";
import { RootSchema } from "./root.ts";

// The base catalog is generic: only `video`/`audio`. Applications add their own root sections
// (e.g. `scte35`) without modifying hang, relying on the loose schema to pass them through.

test("base catalog preserves unknown sections", () => {
	const extended = { video: { renditions: {} }, scte35: { spliceId: 42 } };
	const parsed = RootSchema.parse(extended) as Record<string, unknown>;
	// A base consumer validates the known fields but keeps the unknown section untouched.
	expect(parsed.scte35).toEqual({ spliceId: 42 });
});

test("extended schema validates app sections", () => {
	const Scte35Schema = z.object({ spliceId: z.number() });
	const ExtendedSchema = z.extend(RootSchema, { scte35: z.optional(Scte35Schema) });

	expect(ExtendedSchema.parse({ scte35: { spliceId: 7 } }).scte35).toEqual({ spliceId: 7 });

	// The extended schema enforces the app's section type.
	expect(() => ExtendedSchema.parse({ scte35: { spliceId: "nope" } })).toThrow();
});
