import { expect, test } from "bun:test";
import fc from "fast-check";
import { Decoder, type Event } from "./decoder.ts";
import { type Config, Encoder, type Pending } from "./encoder.ts";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
// `restate` stands for `Encoder.snapshot`, as a refresh does.
type Step = { edit: { push: Json } | { pop: number } | { restate: true }; reset: boolean; drop: boolean };
type Frame = { payload: Uint8Array; offset: number; records: Json[] };

// Mirror the Rust generator, including nulls and keys that are special to JS objects.
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
const steps = fc.array(
	fc.record({
		edit: fc.oneof(
			{ arbitrary: value.map((push) => ({ push })), weight: 4 },
			{ arbitrary: fc.integer({ min: 0, max: 8 }).map((pop) => ({ pop })), weight: 4 },
			{ arbitrary: fc.constant({ restate: true as const }), weight: 1 },
		),
		reset: fc.integer({ min: 0, max: 15 }).map((n) => n === 0),
		drop: fc.boolean(),
	}),
	{ minLength: 1, maxLength: 64 },
);

// Group consecutive indices into the public event ranges without reproducing codec cursors.
function ranges(indices: number[]): Array<{ start: number; end: number }> {
	const result: Array<{ start: number; end: number }> = [];
	for (const index of indices) {
		const last = result.at(-1);
		if (last?.end === index) last.end++;
		else result.push({ start: index, end: index + 1 });
	}
	return result;
}

function check(config: Config, steps: Step[], join: number): void {
	const encoder = new Encoder<Json>(config);
	const groups: Array<{ frames: Frame[]; drop: boolean }> = [];
	let offset = 0;
	let records: Json[] = [];
	for (const step of steps) {
		if (step.reset) encoder.reset();
		let frame: Pending | undefined;
		if ("push" in step.edit) {
			frame = encoder.push(step.edit.push);
			records = [...records, step.edit.push];
		} else if ("restate" in step.edit) {
			frame = encoder.snapshot();
			expect(frame.keyframe).toBe(true);
		} else {
			frame = encoder.pop(step.edit.pop);
			const count = Math.min(step.edit.pop, records.length);
			expect(frame !== undefined).toBe(count > 0);
			offset += count;
			records = records.slice(count);
		}
		if (!frame) {
			expect(encoder.offset).toBe(offset);
			expect(encoder.end).toBe(offset + records.length);
			continue;
		}
		if (frame.keyframe) groups.push({ frames: [], drop: step.drop });
		const group = groups.at(-1);
		if (!group) throw new Error("first frame must open a group");
		group.frames.push({ payload: frame.payload, offset, records });
		frame.commit();
		expect(encoder.offset).toBe(offset);
		expect(encoder.end).toBe(offset + records.length);
	}
	if (groups.length === 0) return;

	for (const mode of ["following", "late", "lagging"] as const) {
		const decoder = new Decoder<Json>();
		const notified = new Set<number>();
		const received = new Set<number>();
		const contents = new Map<number, Json>();
		let timeline: number[] = [];
		let floor: number | undefined;
		const startGroup = mode === "late" ? join % groups.length : 0;
		for (const [g, encoded] of groups.entries()) {
			if (g < startGroup || (mode === "lagging" && g > 0 && g < groups.length - 1 && encoded.drop)) continue;
			const group = decoder.group();
			for (const [f, frame] of encoded.frames.entries()) {
				floor ??= frame.offset;
				const expected: Event<Json>[] = ranges(timeline.filter((index) => index < frame.offset)).map((pop) => ({
					pop,
				}));
				const end = frame.offset + frame.records.length;
				const start =
					f === 0
						? Math.max(frame.offset, end - (config.checkpointRecords ?? frame.records.length))
						: frame.offset;
				const missing: number[] = [];
				if (f === 0) {
					for (let index = floor; index < start; index++) if (!notified.has(index)) missing.push(index);
				}
				for (const skip of ranges(missing)) expected.push({ skip });
				for (const index of missing) notified.add(index);
				for (const [i, value] of frame.records.entries()) {
					const index = frame.offset + i;
					if (index >= start && !notified.has(index)) {
						expected.push({ push: { index, value } });
						notified.add(index);
					}
				}
				group.decode(frame.payload);
				const events: Event<Json>[] = [];
				for (let event = decoder.next(); event; event = decoder.next()) events.push(event);
				expect(events).toEqual(expected);
				for (const event of events) {
					if ("push" in event) {
						expect(received.has(event.push.index)).toBe(false);
						received.add(event.push.index);
						contents.set(event.push.index, event.push.value);
					} else if ("pop" in event) {
						for (let i = event.pop.start; i < event.pop.end; i++) contents.delete(i);
					} else {
						for (let i = event.skip.start; i < event.skip.end; i++) expect(received.has(i)).toBe(false);
					}
				}
				timeline = frame.records.map((_, i) => frame.offset + i);
				expect(decoder.offset).toBe(frame.offset);
				for (const [index, value] of contents) {
					expect(index).toBeGreaterThanOrEqual(frame.offset);
					expect(index).toBeLessThan(end);
					expect(value).toEqual(frame.records[index - frame.offset]);
				}
				if (mode === "following") expect([...contents.values()]).toEqual(frame.records);
			}
		}
	}
}

function setting(name: string, fallback: number, min: number): number {
	const raw = process.env[name];
	if (raw !== undefined && !/^\d+$/.test(raw)) throw new Error(`${name} must be an integer`);
	const value = Number(raw ?? fallback);
	if (!Number.isInteger(value) || value < min || value > 0x7fffffff) {
		throw new Error(`${name} must be an integer between ${min} and 2147483647`);
	}
	return value;
}

test("window codec property matches every reader's logical window", () => {
	fc.assert(
		fc.property(
			fc.record({
				ops: fc.boolean(),
				checkpointRecords: fc.option(fc.integer({ min: 1, max: 8 }), { nil: undefined }),
			}),
			steps,
			fc.nat(63),
			check,
		),
		{ seed: setting("MOQ_JSON_SEED", 0x6d6f71, 0), numRuns: setting("MOQ_JSON_CASES", 256, 1) },
	);
});
