import { expect, test } from "bun:test";
import fc from "fast-check";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type Step = { value: Json; reset: boolean };

// Mirror the Rust generator: bounded JSON trees plus edits to a persistent
// object, so sequences exercise deltas as well as unrelated snapshot roots.
const key = fc.oneof(
	fc.constantFrom("a", "b", "c", "", "__proto__", "constructor", "prototype", "\0", "雪"),
	fc.string({ maxLength: 8 }),
);
const leaf: fc.Arbitrary<Json> = fc.oneof(
	fc.constant(null),
	fc.boolean(),
	fc.integer({ min: -1_000_000, max: 1_000_000 }).map((n) => n / 4),
	fc.oneof(fc.string({ maxLength: 16 }), fc.constant("雪\0")),
);
const value: fc.Arbitrary<Json> = fc.letrec<{ value: Json }>((tie) => ({
	value: fc.oneof(
		{ maxDepth: 4, depthSize: "small", withCrossShrink: true },
		leaf,
		fc.array(tie("value"), { maxLength: 4 }),
		fc.dictionary(key, tie("value"), { maxKeys: 4 }),
	),
})).value;
const reset = fc.integer({ min: 0, max: 15 }).map((n) => n === 0);
const roots = fc.array(fc.record({ value, reset }), { minLength: 1, maxLength: 32 });
const edits = fc
	.array(fc.record({ key, value, remove: fc.boolean(), reset }), { minLength: 1, maxLength: 32 })
	.map((edits): Step[] => {
		let current: Record<string, Json> = {};
		return edits.map(({ key, value, remove, reset }) => {
			const next = { ...current };
			if (remove) delete next[key];
			else Object.defineProperty(next, key, { value, enumerable: true, configurable: true, writable: true });
			current = next;
			return { value: next, reset };
		});
	});

function setting(name: string, fallback: number, min: number): number {
	const raw = process.env[name];
	if (raw !== undefined && !/^\d+$/.test(raw)) throw new Error(`${name} must be an integer`);
	const value = Number(raw ?? fallback);
	if (!Number.isInteger(value) || value < min || value > 0x7fffffff) {
		throw new Error(`${name} must be an integer between ${min} and 2147483647`);
	}
	return value;
}

test("snapshot codec property reconstructs each JSON value", () => {
	const seed = setting("MOQ_JSON_SEED", 0x6d6f71, 0);
	const numRuns = setting("MOQ_JSON_CASES", 256, 1);
	fc.assert(
		fc.property(
			fc.constantFrom(0, 1, 2, 8, 100),
			fc.boolean(),
			fc.oneof(roots, edits),
			(deltaRatio, deflate, steps) => {
				const config = { deltaRatio, compression: deflate ? ("deflate" as const) : ("none" as const) };
				const encoder = new Encoder<Json>(config);
				const decoder = new Decoder<Json>(config);
				for (const { value, reset } of steps) {
					if (reset) encoder.reset();
					const frame = encoder.update(value);
					if (frame) {
						if (frame.keyframe) decoder.snapshot(frame.payload);
						else decoder.delta(frame.payload);
						frame.commit();
					}
					// JSON serialization defines the domain, including its normalization.
					expect(decoder.decode()).toEqual(JSON.parse(JSON.stringify(value)));
				}
			},
		),
		{ seed, numRuns },
	);
});
