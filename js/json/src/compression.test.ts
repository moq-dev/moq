import { expect, test } from "bun:test";
import { Decoder } from "@moq/flate";
import { TrackProducer, type TrackSubscriber } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

const enc = new TextEncoder();
const dec = new TextDecoder();

// Reconstruct every value a compressed consumer yields, in order.
async function drainCompressed(track: TrackSubscriber): Promise<Value[]> {
	const out: Value[] = [];
	for await (const value of new Consumer<Value>(track, { compression: true })) out.push(value);
	return out;
}

// The raw (stored) bytes of a track's first frame, without reconstructing JSON.
async function firstFrame(track: TrackSubscriber): Promise<Uint8Array> {
	const group = await track.nextGroup();
	if (!group) throw new Error("expected a group");
	const frame = await group.readFrame();
	if (!frame) throw new Error("expected a frame");
	return frame.data;
}

// Count the groups a (finished) track published, draining each so the reads terminate.
async function groupCount(track: TrackSubscriber): Promise<number> {
	let groups = 0;
	for (;;) {
		const group = await track.nextGroup();
		if (!group) return groups;
		groups++;
		while ((await group.readFrame()) !== undefined) {}
	}
}

test("compressed snapshot per group round-trips", async () => {
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 0, compression: true });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// Deltas off: one compressed snapshot per group, reconstructed in order.
	expect(await drainCompressed(track.subscribe())).toEqual([{ a: 1 }, { a: 2 }]);
});

test("compressed live consumer sees each update in order", async () => {
	// A live consumer reconstructs each update in order from the shared per-group stream.
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	const consumer = new Consumer<Value>(track.subscribe(), { compression: true });

	for (let n = 1; n <= 5; n++) {
		producer.update({ a: n });
		expect(await consumer.next()).toEqual({ a: n });
	}
});

test("compressed deltas share one group and reconstruct", async () => {
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	expect((await drainCompressed(track.subscribe())).at(-1)).toEqual({ a: 5, b: 2 });
});

test("compressed late joiner reconstructs from snapshot + deltas", async () => {
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	// A consumer created only now still rebuilds the final value from the snapshot + deltas.
	expect((await drainCompressed(track.subscribe())).at(-1)).toEqual({ a: 5, b: 2 });
});

test("a group's snapshot decodes from a fresh decoder", async () => {
	// Frame 0 opens a cold window, so a brand-new decoder reconstructs it, which is what lets a late
	// joiner (or the Rust consumer) start mid-stream at any group boundary.
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 0, compression: true });
	producer.update({ hello: "world" });
	producer.finish();

	const decoder = new Decoder();
	expect(JSON.parse(dec.decode(decoder.frame(await firstFrame(track.subscribe()))))).toEqual({ hello: "world" });
});

test("compressed deltas reuse the window", async () => {
	// The shared per-group window is the point: a delta restating snapshot content shrinks sharply.
	const track = new TrackProducer("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, compression: true });
	const phrase = "Media over QUIC delivers real-time latency at massive scale";
	producer.update({ note: phrase });
	producer.update({ note: phrase, echo: phrase });
	producer.finish();

	const group = await track.subscribe().nextGroup();
	if (!group) throw new Error("expected a group");
	await group.readFrame(); // snapshot
	const delta = await group.readFrame();
	if (!delta) throw new Error("expected a delta");

	const rawDelta = enc.encode(JSON.stringify({ echo: phrase }));
	expect(delta.data.length).toBeLessThan(rawDelta.length / 2);
});

test("compression shrinks a repetitive frame", async () => {
	const value = { renditions: Array(3).fill("video".repeat(50)) };

	const plain = new TrackProducer("plain");
	new Producer<Value>(plain, { deltaRatio: 0 }).update(value);
	const compressed = new TrackProducer("compressed");
	new Producer<Value>(compressed, { deltaRatio: 0, compression: true }).update(value);

	const plainLen = (await firstFrame(plain.subscribe())).length;
	const compressedLen = (await firstFrame(compressed.subscribe())).length;
	expect(compressedLen).toBeLessThan(plainLen);
});

test("compressed deltas roll on the compressed budget", async () => {
	// With compression the budget is measured on compressed frame sizes: #snapshotLen and #deltaBytes
	// are the written slice lengths, not the raw JSON. A tight ratio over many distinct updates must
	// roll more than one group, and a late joiner must still rebuild the final value across the
	// compressed group boundary. Guards against the budget regressing to raw lengths. Two identical
	// producers (deterministic output) keep the group-count and reconstruction reads independent.
	const fill = (track: TrackProducer) => {
		const producer = new Producer<Value>(track, { deltaRatio: 2, compression: true });
		for (let n = 0; n <= 40; n++) producer.update({ n });
		producer.finish();
	};

	const layout = new TrackProducer("layout");
	const layoutSub = layout.subscribe();
	fill(layout);
	expect(await groupCount(layoutSub)).toBeGreaterThan(1);

	const reconstruct = new TrackProducer("reconstruct");
	const reconstructSub = reconstruct.subscribe();
	fill(reconstruct);
	expect((await drainCompressed(reconstructSub)).at(-1)).toEqual({ n: 40 });
});
