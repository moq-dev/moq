import { expect, test } from "bun:test";
import { Decoder } from "./decoder.ts";
import { type Config, Encoder } from "./encoder.ts";

type Doc = Record<string, unknown>;

// Encode a sequence of values, committing each frame, and return `[keyframe, payloadLength]` per
// emitted frame.
function encode(config: Config<Doc>, values: Doc[]): [boolean, number][] {
	const encoder = new Encoder<Doc>(config);
	const out: [boolean, number][] = [];
	for (const value of values) {
		const frame = encoder.update(value);
		if (frame) {
			out.push([frame.keyframe, frame.payload.length]);
			frame.commit();
		}
	}
	return out;
}

// Encode one value and commit it.
function commit(encoder: Encoder<Doc>, value: Doc) {
	const frame = encoder.update(value);
	frame?.commit();
	return frame;
}

// Round-trip a sequence through an encoder and decoder, yielding what the decoder reconstructs
// after each frame.
function roundtrip(config: Config<Doc>, values: Doc[]): Doc[] {
	const encoder = new Encoder<Doc>(config);
	const decoder = new Decoder<Doc>(config);

	const out: Doc[] = [];
	for (const value of values) {
		const frame = encoder.update(value);
		if (!frame) continue;
		if (frame.keyframe) {
			decoder.snapshot(frame.payload);
		} else {
			decoder.delta(frame.payload);
		}
		frame.commit();
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
	expect(commit(encoder, { a: 1 })?.keyframe).toBe(true);
	expect(commit(encoder, { a: 2 })?.keyframe).toBe(false);

	encoder.reset();
	expect(commit(encoder, { a: 3 })?.keyframe).toBe(true);
});

// A reset value is republished even when it matches the last one encoded: the new group has to open
// with a snapshot, so "unchanged" can't mean "write nothing" there.
test("reset republishes an unchanged value", () => {
	const encoder = new Encoder<Doc>({});
	commit(encoder, { a: 1 });

	encoder.reset();
	expect(commit(encoder, { a: 1 })?.keyframe).toBe(true);
});

test("the encoder tracks the published baseline", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	expect(encoder.value).toBeUndefined();

	commit(encoder, { a: 1, b: 1 });
	commit(encoder, { a: 1, b: 2 });

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
		const frame = encoder.update({ n });
		if (!frame) continue;
		if (frame.keyframe) {
			decoder.snapshot(frame.payload);
		} else {
			decoder.delta(frame.payload);
		}
		frame.commit();
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

// A frame the caller never wrote must not leave the encoder emitting deltas against a baseline no
// consumer received. Not committing is what a failed write looks like, and the encoder has to
// resynchronize on its own: a caller cannot be relied on to remember. (Rust does this at drop; JS
// has no destructor, so it happens on the next update.)
test("an uncommitted frame resynchronizes the encoder", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	commit(encoder, { a: 1 });
	expect(commit(encoder, { a: 2 })?.keyframe).toBe(false);

	// This one fails to write, so the caller never commits it.
	expect(encoder.update({ a: 3 })?.keyframe).toBe(false);

	// The next value opens a new group with a full snapshot rather than patching a state the
	// consumer never reached.
	const recovered = commit(encoder, { a: 4 });
	expect(recovered?.keyframe).toBe(true);
	expect(JSON.parse(new TextDecoder().decode(recovered?.payload))).toEqual({ a: 4 });
});

// The same recovery when the very first frame is lost: the encoder must not treat the value as
// already published and skip it as unchanged.
test("an uncommitted first frame is re-encoded", () => {
	const encoder = new Encoder<Doc>({});
	encoder.update({ a: 1 });

	expect(commit(encoder, { a: 1 })?.keyframe).toBe(true);
});

// The baseline is what the next delta is diffed against, so handing out the live object lets a
// caller mutate it and make a real change look unchanged, silently starving consumers.
test("the value getter does not expose the live baseline", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	commit(encoder, { a: 1 });

	const leaked = encoder.value as { a: number };
	leaked.a = 2;

	// The change is still a change, because the mutation never touched the encoder's copy.
	expect(commit(encoder, { a: 2 })).toBeDefined();
});

test("a value JSON cannot represent is rejected", () => {
	const encoder = new Encoder<unknown>({});
	expect(() => encoder.update(undefined)).toThrow("not representable as JSON");
});

// A caller that starts the next update before the previous frame settles makes the encoder
// resynchronize around it. The stale commit landing afterwards must not clear the flag belonging to
// the newer frame, or a later loss of that one goes unnoticed. (Rust cannot express this: its
// `Pending` borrows the encoder, so a second `update` while one is outstanding does not compile.)
test("a stale commit does not clear a newer pending frame", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	const first = encoder.update({ a: 1 });

	// The second update starts before the first frame settles, so the encoder resyncs around it.
	expect(encoder.update({ a: 2 })?.keyframe).toBe(true);

	// The first write finally completes, but it is no longer the outstanding frame.
	first?.commit();

	// The second frame was never committed either, so this must still resynchronize.
	expect(encoder.update({ a: 3 })?.keyframe).toBe(true);
});

// The baseline is also what `Producer.mutate` seeds an edit from, so a resync must not erase it:
// the next edit would otherwise publish a document with every other field missing.
test("a resync keeps the value for mutate", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	commit(encoder, { video: "v1", scte35: 1 });

	encoder.reset();
	expect(encoder.value).toEqual({ video: "v1", scte35: 1 });
});

// The reconstruction is the base every later delta merges into, so handing out the live object lets
// a caller change fields that no frame ever carried.
test("the decoder does not expose its live reconstruction", () => {
	const encoder = new Encoder<Doc>({ deltaRatio: 100 });
	const decoder = new Decoder<Doc>({});

	const snapshot = commit(encoder, { a: 1, b: 1 });
	decoder.snapshot(snapshot?.payload as Uint8Array);

	const leaked = decoder.decode() as { a: number };
	leaked.a = 99;

	// An unrelated delta must not carry the caller's mutation along with it.
	const delta = commit(encoder, { a: 1, b: 2 });
	decoder.delta(delta?.payload as Uint8Array);
	expect(decoder.decode()).toEqual({ a: 1, b: 2 });
});

// Finishing takes the open group with it, so the encoder must stop emitting deltas into a group that
// no longer exists. A later update has to surface the closed track rather than a delta with nowhere
// to put it. (Mirrors the Rust `update_after_finish_errors_instead_of_panicking`.)
test("update after finish reports the closed track", async () => {
	const { Track } = await import("@moq/net");
	const { Producer } = await import("./producer.ts");

	const track = new Track.Producer("test");
	const producer = new Producer<Doc>({ track, deltaRatio: 100 });
	producer.update({ a: 1 });
	producer.update({ a: 2 }); // a delta, so a group is open
	producer.finish();

	expect(() => producer.update({ a: 3 })).toThrow("track is closed");
});

// A root that isn't an object has no recursive merge patch, so it forces a snapshot regardless of
// the delta budget. Matches the Rust `a_non_object_root_forces_a_keyframe`.
test("a non-object root forces a keyframe", () => {
	// Typed as `unknown` because the root deliberately stops being an object partway through.
	const encoder = new Encoder<unknown>({ deltaRatio: 100 });

	const object = encoder.update({ a: 1 });
	object?.commit();
	const array = encoder.update([1, 2, 3]);
	array?.commit();

	expect([object?.keyframe, array?.keyframe]).toEqual([true, true]);
});

// The per-group frame cap rolls a new snapshot even when the byte budget is nowhere near spent, so a
// late joiner can always read frame 0. Matches the Rust `frame_cap_forces_a_keyframe`.
test("the frame cap forces a keyframe", () => {
	const values: Doc[] = Array.from({ length: 257 }, (_, n) => ({ n }));
	const frames = encode({ deltaRatio: 1_000_000 }, values);

	expect(frames.length).toBe(257);
	expect(frames.filter((f) => f[0]).length).toBe(2);
	expect(frames[256][0]).toBe(true);
});
