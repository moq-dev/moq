import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Decoder, Encoder } from "./compression.ts";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

const enc = new TextEncoder();
const dec = new TextDecoder();

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
	const encoder = await Encoder.create();
	const slices = frames.map((f) => encoder.frame(enc.encode(f)));

	const decoder = await Decoder.create();
	expect(slices.map((s) => dec.decode(decoder.frame(s)))).toEqual(frames);
});

test("codec round-trips an empty frame", async () => {
	const encoder = await Encoder.create();
	const decoder = await Decoder.create();
	expect(encoder.frame(new Uint8Array()).length).toBe(0);
	expect(decoder.frame(new Uint8Array()).length).toBe(0);
});

test("codec rejects garbage", async () => {
	const decoder = await Decoder.create();
	expect(() => decoder.frame(new Uint8Array(64).fill(0xff))).toThrow();
});

test("cross-frame context shrinks a repeated frame", async () => {
	// A later frame identical to an earlier one compresses far smaller once the window holds it.
	const encoder = await Encoder.create();
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
	// Compression makes writes async, so this exercises that the streaming pipeline still delivers
	// frames (and groups) strictly in order.
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

	const decoder = await Decoder.create();
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
