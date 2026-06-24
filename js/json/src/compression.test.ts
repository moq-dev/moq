import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Deflate, Inflate } from "fflate";
import { Decoder, Encoder } from "./compression.ts";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

const enc = new TextEncoder();
const dec = new TextDecoder();

function concatBytes(chunks: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out;
}

// Round-trip frames through fflate's streaming `Deflate.flush(true)` + `Inflate`, the same
// shared-window scheme our pako codec uses. Returns true only if every frame survives unchanged.
function fflateRoundTrips(frames: Uint8Array[]): boolean {
	try {
		let captured: Uint8Array[] = [];
		const deflate = new Deflate({ level: 6 });
		deflate.ondata = (chunk) => captured.push(chunk);
		const slices = frames.map((frame) => {
			captured = [];
			deflate.push(frame, false);
			deflate.flush(true); // sync flush: byte-align and retain the window
			return concatBytes(captured);
		});

		let inflated: Uint8Array[] = [];
		const inflate = new Inflate();
		inflate.ondata = (chunk) => inflated.push(chunk);
		return slices.every((slice, i) => {
			inflated = [];
			inflate.push(slice, false);
			const got = concatBytes(inflated);
			return got.length === frames[i].length && got.every((b, j) => b === frames[i][j]);
		});
	} catch {
		return false;
	}
}

// Reconstruct every value a compressed consumer yields, in order.
async function drainCompressed(track: Track): Promise<Value[]> {
	const out: Value[] = [];
	for await (const value of new Consumer<Value>(track, { compression: true })) out.push(value);
	return out;
}

// The raw (stored) bytes of a track's first frame, without reconstructing JSON.
async function firstFrame(track: Track): Promise<Uint8Array> {
	const group = await track.nextGroupOrdered();
	if (!group) throw new Error("expected a group");
	const frame = await group.readFrame();
	if (!frame) throw new Error("expected a frame");
	return frame;
}

test("codec round-trips a group of frames in order", async () => {
	const frames = ["the quick brown fox", "the quick brown dog", "the lazy fox"];
	const encoder = new Encoder();
	const slices = frames.map((f) => encoder.frame(enc.encode(f)));

	const decoder = new Decoder();
	expect(slices.map((s) => dec.decode(decoder.frame(s)))).toEqual(frames);
});

test("codec round-trips an empty frame", async () => {
	const encoder = new Encoder();
	const decoder = new Decoder();
	expect(encoder.frame(new Uint8Array()).length).toBe(0);
	expect(decoder.frame(new Uint8Array()).length).toBe(0);
});

test("codec rejects garbage", async () => {
	const decoder = new Decoder();
	expect(() => decoder.frame(new Uint8Array(64).fill(0xff))).toThrow();
});

test("codec rejects frames that inflate past the cap", async () => {
	// A tiny slice can inflate enormously, so the decoder bounds the output as it is produced.
	const encoder = new Encoder();
	const decoder = new Decoder();
	const slice = encoder.frame(enc.encode("a".repeat(64 * 1024 * 1024 + 1)));
	expect(() => decoder.frame(slice)).toThrow(/exceeded/);
});

test("a frame larger than pako's chunk size round-trips", () => {
	// High-entropy data barely compresses, so the slice spans multiple pako chunks (>16 KB), which
	// exercises the encoder's multi-chunk assembly and the decoder's multi-chunk concat.
	let state = 0x9e3779b9 >>> 0;
	const payload = new Uint8Array(64 * 1024);
	for (let i = 0; i < payload.length; i++) {
		state ^= state << 13;
		state ^= state >>> 17;
		state ^= state << 5;
		state >>>= 0;
		payload[i] = state & 0xff;
	}

	const slice = new Encoder().frame(payload);
	expect(slice.length).toBeGreaterThan(16 * 1024); // pako's default chunkSize
	expect(new Decoder().frame(slice)).toEqual(payload);
});

test("cross-frame context shrinks a repeated frame", async () => {
	// A later frame identical to an earlier one compresses far smaller once the window holds it.
	const encoder = new Encoder();
	const payload = enc.encode("Media over QUIC delivers real-time latency at massive scale.".repeat(6));
	const first = encoder.frame(payload);
	const second = encoder.frame(payload);
	expect(second.length).toBeLessThan(first.length);
});

test("compressed snapshot per group round-trips", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 0, compression: true });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// Deltas off: one compressed snapshot per group, reconstructed in order.
	expect(await drainCompressed(track)).toEqual([{ a: 1 }, { a: 2 }]);
});

test("compressed live consumer sees each update in order", async () => {
	// A live consumer reconstructs each update in order from the shared per-group stream.
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	const consumer = new Consumer<Value>(track, { compression: true });

	for (let n = 1; n <= 5; n++) {
		producer.update({ a: n });
		expect(await consumer.next()).toEqual({ a: n });
	}
});

test("compressed deltas share one group and reconstruct", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	expect((await drainCompressed(track)).at(-1)).toEqual({ a: 5, b: 2 });
});

test("compressed late joiner reconstructs from snapshot + deltas", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	// A consumer created only now still rebuilds the final value from the snapshot + deltas.
	expect((await drainCompressed(track)).at(-1)).toEqual({ a: 5, b: 2 });
});

test("a group's snapshot decodes from a fresh decoder", async () => {
	// Frame 0 opens a cold window, so a brand-new decoder reconstructs it, which is what lets a late
	// joiner (or the Rust consumer) start mid-stream at any group boundary.
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 0, compression: true });
	producer.update({ hello: "world" });
	producer.finish();

	const decoder = new Decoder();
	expect(JSON.parse(dec.decode(decoder.frame(await firstFrame(track))))).toEqual({ hello: "world" });
});

test("compressed deltas reuse the window", async () => {
	// The shared per-group window is the point: a delta restating snapshot content shrinks sharply.
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	const phrase = "Media over QUIC delivers real-time latency at massive scale";
	producer.update({ note: phrase });
	producer.update({ note: phrase, echo: phrase });
	producer.finish();

	const group = await track.nextGroupOrdered();
	if (!group) throw new Error("expected a group");
	await group.readFrame(); // snapshot
	const delta = await group.readFrame();
	if (!delta) throw new Error("expected a delta");

	const rawDelta = enc.encode(JSON.stringify({ echo: phrase }));
	expect(delta.length).toBeLessThan(rawDelta.length / 2);
});

test("pako round-trips a group that fflate's flush corrupts", async () => {
	// A catalog snapshot + 3 deltas that fflate's streaming flush mis-encodes: even fflate's own
	// Inflate can't round-trip its output here. This pins why @moq/json depends on pako, not the
	// smaller fflate. If this ever fails (fflateRoundTrips returns true), fflate may have fixed its
	// sync-flush encoder, and dropping the pako dependency could be reconsidered.
	const group: Value[] = [
		{
			video: {
				renditions: {
					v0: { codec: "avc1.64001f", bitrate: 6000000 },
					v1: { codec: "avc1.640015", bitrate: 3000000 },
				},
			},
			audio: { renditions: { a0: { codec: "opus", bitrate: 128000 } } },
		},
		{ video: { renditions: { v0: { bitrate: 6200000 } } } },
		{ video: { renditions: { v0: { bitrate: 5800000 } } } },
		{ audio: { renditions: { a0: { bitrate: 96000 } } } },
	];
	const frames = group.map((value) => enc.encode(JSON.stringify(value)));

	// Our pako codec round-trips every frame of the group exactly.
	const encoder = new Encoder();
	const decoder = new Decoder();
	for (const frame of frames) {
		expect(decoder.frame(encoder.frame(frame))).toEqual(frame);
	}

	// Positive control: fflate's flush works on simpler frames, so the helper is sound and fflate is
	// only selectively broken, not failing for some unrelated reason.
	expect(fflateRoundTrips(["the quick brown fox", "the quick brown dog"].map((s) => enc.encode(s)))).toBe(true);

	// fflate's streaming flush does not round-trip the same group our pako codec handles.
	expect(fflateRoundTrips(frames)).toBe(false);
});

test("compression shrinks a repetitive frame", async () => {
	const value = { renditions: Array(3).fill("video".repeat(50)) };

	const plain = new Track("plain");
	new Producer<Value>(plain, { deltaRatio: 0 }).update(value);
	const compressed = new Track("compressed");
	new Producer<Value>(compressed, { deltaRatio: 0, compression: true }).update(value);

	const plainLen = (await firstFrame(plain)).length;
	const compressedLen = (await firstFrame(compressed)).length;
	expect(compressedLen).toBeLessThan(plainLen);
});
