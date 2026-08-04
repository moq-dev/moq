import { expect, test } from "bun:test";
import { Decoder } from "./decoder.ts";
import { type Config, Encoder } from "./encoder.ts";

type Doc = Record<string, unknown>;

// Encode a sequence of values, returning `[keyframe, payloadLength]` per emitted frame.
function encode(config: Config<Doc>, values: Doc[]): [boolean, number][] {
	const encoder = new Encoder<Doc>(config);
	const out: [boolean, number][] = [];
	for (const value of values) {
		const encoded = encoder.update(value);
		if (encoded) out.push([encoded.keyframe, encoded.payload.length]);
	}
	return out;
}

// Round-trip a sequence through an encoder and decoder, yielding what the decoder reconstructs
// after each frame.
function roundtrip(config: Config<Doc>, values: Doc[]): Doc[] {
	const encoder = new Encoder<Doc>(config);
	const decoder = new Decoder<Doc>(config);

	const out: Doc[] = [];
	for (const value of values) {
		const encoded = encoder.update(value);
		if (!encoded) continue;
		if (encoded.keyframe) {
			decoder.snapshot(encoded.payload);
		} else {
			decoder.delta(encoded.payload);
		}
		out.push(decoder.decode() as Doc);
	}
	return out;
}

test("the first update is a keyframe", () => {
	expect(encode({}, [{ a: 1 }])).toEqual([[true, 7]]);
});

test("an unchanged value encodes nothing", () => {
	expect(encode({}, [{ a: 1 }, { a: 1 }]).length).toBe(1);
});

test("changes ride as deltas", () => {
	const frames = encode({ deltaRatio: 100 }, [
		{ a: 1, b: 1 },
		{ a: 1, b: 2 },
		{ a: 1, b: 3 },
	]);
	expect(frames.map((f) => f[0])).toEqual([true, false, false]);
});

test("deltas off forces a keyframe per change", () => {
	const frames = encode({ deltaRatio: 0 }, [{ a: 1 }, { a: 2 }]);
	expect(frames.map((f) => f[0])).toEqual([true, true]);
});

// A value the caller might reasonably expect to be a delta, but that merge patch can't express:
// setting a field to JSON null reads as a key deletion. The encoder has to override the caller here,
// which is why `keyframe` is a return value rather than a parameter.
test("a null field forces a keyframe", () => {
	const frames = encode({ deltaRatio: 100 }, [
		{ a: 1, b: 1 },
		{ a: 1, b: null },
	]);
	expect(frames.map((f) => f[0])).toEqual([true, true]);
});

// A caller that closes the group behind the encoder's back has to say so, or the next value would be
// a delta against a window and a baseline the new group never carried.
test("reset forces the next update to be a keyframe", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	expect(encoder.update({ a: 1 })?.keyframe).toBe(true);
	expect(encoder.update({ a: 2 })?.keyframe).toBe(false);

	encoder.reset();
	expect(encoder.update({ a: 3 })?.keyframe).toBe(true);
});

// A reset value is republished even when it matches the last one encoded: the new group has to open
// with a snapshot, so "unchanged" can't mean "write nothing" there.
test("reset republishes an unchanged value", () => {
	const encoder = new Encoder<Doc>({});
	encoder.update({ a: 1 });

	encoder.reset();
	expect(encoder.update({ a: 1 })?.keyframe).toBe(true);
});

test("the encoder tracks the published baseline", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	expect(encoder.value).toBeUndefined();

	encoder.update({ a: 1, b: 1 });
	encoder.update({ a: 1, b: 2 });

	// The delta was folded into the baseline, so it reflects what was actually published.
	expect(encoder.value).toEqual({ a: 1, b: 2 });
});

test("plaintext roundtrip", () => {
	const values: Doc[] = [
		{ a: 1, b: 1 },
		{ a: 1, b: 2 },
		{ a: 5, b: 2 },
	];
	expect(roundtrip({}, values)).toEqual(values);
});

test("compressed roundtrip", () => {
	const values: Doc[] = [
		{ a: 1, b: 1 },
		{ a: 1, b: 2 },
		{ a: 5, b: 2 },
	];
	expect(roundtrip({ compression: true }, values)).toEqual(values);
});

// The window is per group, so a keyframe mid-stream has to restart it on both sides. A decoder that
// kept the old window here would fail to inflate the new group's snapshot.
test("compressed roundtrip across a group boundary", () => {
	const values: Doc[] = Array.from({ length: 41 }, (_, n) => ({ n }));
	const got = roundtrip({ deltaRatio: 2, compression: true }, values);
	expect(got.at(-1)).toEqual({ n: 40 });
});

test("no value before the first snapshot", () => {
	const decoder = new Decoder<Doc>({});
	expect(decoder.value).toBeUndefined();
	expect(decoder.decode()).toBeUndefined();
});

test("a delta before a snapshot throws", () => {
	const decoder = new Decoder<Doc>({});
	expect(() => decoder.delta(new TextEncoder().encode('{"a":1}'))).toThrow("delta before snapshot");
});

// A backlog is applied in full but materialized once: the intermediate reconstructions are stale.
test("frames apply without materializing", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	const decoder = new Decoder<Doc>({});

	for (let n = 0; n <= 20; n += 1) {
		const encoded = encoder.update({ n });
		if (!encoded) continue;
		if (encoded.keyframe) {
			decoder.snapshot(encoded.payload);
		} else {
			decoder.delta(encoded.payload);
		}
	}

	expect(decoder.decode()).toEqual({ n: 20 });
});

test("compressed deltas reuse the group window", () => {
	const phrase = "Media over QUIC delivers real-time latency at massive scale";
	const frames = encode({ deltaRatio: 100, compression: true }, [{ note: phrase }, { note: phrase, echo: phrase }]);

	// The raw patch repeats the whole phrase; compressed against the window it's a fraction.
	const raw = new TextEncoder().encode(JSON.stringify({ echo: phrase })).length;
	expect(frames.length).toBe(2);
	expect(frames[1][1]).toBeLessThan(raw / 2);
});
