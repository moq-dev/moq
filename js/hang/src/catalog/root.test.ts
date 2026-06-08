import { expect, test } from "bun:test";
import * as z from "zod/mini";
import { RootSchema } from "./root.ts";

// An application-defined section, the kind that lives in the app layer (e.g. hang.live) rather
// than in the base catalog.
const Scte35Schema = z.object({
	track: z.string(),
	spliceCount: z.optional(z.number()),
});

// Compose the base catalog with the extension, exactly as an application would.
const ExtendedSchema = z.extend(RootSchema, { scte35: z.optional(Scte35Schema) });

test("base catalog drops unknown sections", () => {
	// The base schema is closed: an app section round-trips to nothing, so you must extend it.
	expect(RootSchema.parse({ scte35: { track: "splice.json" } })).toEqual({});
});

test("extended catalog preserves the section and the base fields", () => {
	const parsed = ExtendedSchema.parse({
		audio: { renditions: {} },
		scte35: { track: "splice.json", spliceCount: 2 },
	});
	expect(parsed.audio).toEqual({ renditions: {} });
	expect(parsed.scte35).toEqual({ track: "splice.json", spliceCount: 2 });
});

test("extended catalog still rejects an invalid section", () => {
	expect(() => ExtendedSchema.parse({ scte35: { spliceCount: 1 } })).toThrow();
});
