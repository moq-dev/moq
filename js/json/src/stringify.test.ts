import { beforeEach, expect, test } from "bun:test";
import { Track } from "@moq/net";
import fc from "fast-check";
import { key, resetKeys } from "./key.test-util.ts";
import { parse, stringify } from "./parse.ts";
import * as Snapshot from "./snapshot/index.ts";
import * as Stream from "./stream/index.ts";
import * as Window from "./window/index.ts";

beforeEach(resetKeys);

const encode = (text: string) => new TextEncoder().encode(text);

// `levels` arrays nested around `inner`.
function nest(levels: number, inner: unknown = 0): unknown {
	let value = inner;
	for (let i = 0; i < levels; i++) value = [value];
	return value;
}

// Values every reader refuses, whichever writer frames them.
const refused: [string, unknown][] = [
	["a lone high surrogate", "a\ud800b"],
	["a lone low surrogate", { text: "\udfff" }],
	["a reversed pair", ["\udc00\ud800"]],
	["a lone surrogate in a key", { "\udbff": 1 }],
	["128 nested arrays", nest(128)],
	["128 nested objects", JSON.parse(`${'{"a":'.repeat(127)}{}${"}".repeat(127)}`)],
];

// Near misses the scan must not mistake for a refused value.
const accepted: [string, unknown][] = [
	["a surrogate pair", "雪😀"],
	["an escaped backslash before ud800 text", "\\ud800"],
	["an escaped quote and backslash", 'a\\"[[[{'],
	["brackets inside a long string", "[".repeat(300)],
	["escaped quotes inside a long string", `${'\\"'.repeat(40)}${"{".repeat(300)}`],
	["127 levels under long keys", nest(126, { ["\\".repeat(40)]: '"'.repeat(40) })],
];

// `stringify` succeeds exactly when the frame it builds, `envelope` containers deep, parses.
test("stringify accepts what parse accepts inside any envelope", () => {
	const unit = fc.oneof(
		fc.constantFrom("\ud800", "\udfff", "😀", "\\", '"', "u", "d", "8", "\\ud800", "[", "{"),
		fc.integer({ min: 0, max: 0xffff }).map((code) => String.fromCharCode(code)),
	);
	// Past 32 characters the scan searches for a string's end instead of stepping through it.
	const text = fc.oneof(fc.string({ unit, maxLength: 8 }), fc.string({ unit, minLength: 30, maxLength: 48 }));
	const leaf = fc.oneof(fc.constant(null), fc.boolean(), fc.double(), text);
	const value = fc.letrec<{ value: unknown }>((tie) => ({
		value: fc.oneof(
			{ maxDepth: 3 },
			leaf,
			fc.array(tie("value"), { maxLength: 3 }),
			fc.dictionary(text, tie("value"), { maxKeys: 3 }),
		),
	})).value;
	// Wrap a small value in enough arrays to straddle the limit.
	const deep = fc.tuple(fc.integer({ min: 120, max: 130 }), value).map(([levels, inner]) => nest(levels, inner));

	fc.assert(
		fc.property(fc.oneof(value, deep), fc.integer({ min: 0, max: 3 }), (value, envelope) => {
			const framed = `${"[".repeat(envelope)}${JSON.stringify(value)}${"]".repeat(envelope)}`;
			let readable = true;
			try {
				parse(encode(framed));
			} catch {
				readable = false;
			}
			let written = true;
			try {
				expect(stringify(value, envelope)).toBe(JSON.stringify(value));
			} catch {
				written = false;
			}
			expect(written).toBe(readable);
		}),
		{ numRuns: 2000 },
	);
});

test("each encoder refuses what its decoder refuses and keeps encoding", () => {
	for (const [name, value] of [...refused, ["a 126-deep window record", nest(126)] as const]) {
		const window = new Window.Encoder<unknown>();
		expect(() => window.push(value), name).toThrow(TypeError);

		// Nothing moved, so the next record opens the window at index 0 with a header.
		const frame = window.push(1);
		expect(frame.keyframe).toBe(true);
		frame.commit();
		const group = new Window.Decoder<unknown>().group();
		group.decode(frame.payload);
		expect(window.end).toBe(1);
	}

	for (const [name, value] of refused) {
		const stream = new Stream.Encoder<unknown>();
		expect(() => stream.encode(value), name).toThrow(TypeError);
		expect(new Stream.Decoder<unknown>().decode(stream.encode(1).payload)).toBe(1);

		const snapshot = new Snapshot.Encoder<unknown>();
		const first = snapshot.update({ n: 0 });
		first?.commit();
		expect(() => snapshot.update(value), name).toThrow(TypeError);

		// The refused value never became the baseline, so the next change is a delta against the first.
		const next = snapshot.update({ n: 1 });
		expect(next?.keyframe).toBe(false);
		const decoder = new Snapshot.Decoder<unknown>();
		decoder.snapshot(first?.payload ?? new Uint8Array());
		decoder.delta(next?.payload ?? new Uint8Array());
		expect(decoder.decode()).toEqual({ n: 1 });
	}
});

test("the deepest record each encoder accepts decodes", () => {
	const stream = nest(127);
	expect(new Stream.Decoder<unknown>().decode(new Stream.Encoder<unknown>().encode(stream).payload)).toEqual(stream);

	const snapshot = new Snapshot.Decoder<unknown>();
	const encoder = new Snapshot.Encoder<unknown>();
	const first = encoder.update({ a: nest(126) });
	first?.commit();
	snapshot.snapshot(first?.payload ?? new Uint8Array());
	// A delta nests no deeper than the value it patches.
	const delta = encoder.update({ a: nest(126, 1) });
	expect(delta?.keyframe).toBe(false);
	snapshot.delta(delta?.payload ?? new Uint8Array());
	expect(snapshot.decode()).toEqual({ a: nest(126, 1) });

	// A window record sits two containers deep in a header and one in an op; both decode.
	const window = new Window.Encoder<unknown>();
	const record = nest(125);
	const header = window.push(record);
	header.commit();
	const op = window.push(record);
	expect(op.keyframe).toBe(false);
	op.commit();
	const group = new Window.Decoder<unknown>().group();
	group.decode(header.payload);
	group.decode(op.payload);
	const restated = window.snapshot();
	new Window.Decoder<unknown>().group().decode(restated.payload);

	for (const value of accepted.map(([, value]) => value)) {
		expect(new Stream.Decoder<unknown>().decode(new Stream.Encoder<unknown>().encode(value).payload)).toEqual(
			value,
		);
	}
});

test("producers publish only records their consumers read", async () => {
	for (const [name, value] of refused) {
		// A snapshot keeps its last value and takes the next one.
		const snapshotTrack = new Track.Producer("snapshot");
		const snapshot = new Snapshot.Producer<unknown>({ track: snapshotTrack });
		snapshot.update({ n: 0 }, key());
		expect(() => snapshot.update(value, key()), name).toThrow(TypeError);
		snapshot.update({ n: 1 }, key());
		snapshot.finish();
		const snapshots = new Snapshot.Consumer<unknown>({ track: snapshotTrack });
		expect((await snapshots.latest())?.value, name).toEqual({ n: 1 });

		// A window keeps its records and takes the next one.
		const windowTrack = new Track.Producer("window");
		const window = new Window.Producer<unknown>({ track: windowTrack });
		window.push(0, key());
		expect(() => window.push(value, key()), name).toThrow(TypeError);
		window.push(1, key());
		window.finish();
		const windows = new Window.Consumer<unknown>({ track: windowTrack.subscribe() });
		const events: unknown[] = [];
		for (let event = await windows.next(); event; event = await windows.next()) events.push(event.value);
		expect(events, name).toEqual([{ push: { index: 0, value: 0 } }, { push: { index: 1, value: 1 } }]);

		// A log missing a record ends with the refusal rather than a frame its reader rejects.
		const streamTrack = new Track.Producer("stream");
		const stream = new Stream.Producer<unknown>({ track: streamTrack });
		stream.append(0, key());
		expect(() => stream.append(value, key()), name).toThrow(TypeError);
		const records = new Stream.Consumer<unknown>({ track: streamTrack.subscribe() });
		expect((await records.next())?.value, name).toBe(0);
		await expect(records.next(), name).rejects.toThrow(TypeError);
	}
});
