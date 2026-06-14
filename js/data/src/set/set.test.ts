import { expect, test } from "bun:test";
import { Track } from "@moq/net";

import { stringCodec } from "./codec.ts";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

// Reconstruct every set a consumer yields, in order. Consumes the track's groups.
async function drain(track: Track): Promise<Set<string>[]> {
	const out: Set<string>[] = [];
	for await (const value of new Consumer(track, { codec: stringCodec })) out.push(value);
	return out;
}

// Inspect the published layout via the public API: the frame count of each group, in order. Like
// `drain`, this consumes the track's groups, so don't call both on one track. Finish the track
// first so group/frame reads terminate.
async function structure(track: Track): Promise<number[]> {
	const counts: number[] = [];
	for (;;) {
		const group = await track.nextGroupOrdered();
		if (!group) break;

		let frames = 0;
		while ((await group.readFrame()) !== undefined) frames++;
		counts.push(frames);
	}
	return counts;
}

function set(...items: string[]): Set<string> {
	return new Set(items);
}

test("deltas off: a snapshot group per change", async () => {
	const track = new Track("test");
	// A tight ratio leaves no room for any delta past the snapshot, so every change rolls a group.
	const producer = new Producer(track, { codec: stringCodec, deltaRatio: 0 });
	producer.insert("video");
	producer.insert("audio");
	producer.finish();

	expect((await drain(track)).at(-1)).toEqual(set("video", "audio"));
});

test("deltas share one group", async () => {
	const track = new Track("test");
	const producer = new Producer(track, { codec: stringCodec });
	producer.insert("video"); // snapshot
	producer.insert("audio"); // delta
	producer.remove("video"); // delta
	producer.finish();

	// All changes fit in a single group as snapshot + two deltas.
	expect(await structure(track)).toEqual([3]);
});

test("redundant insert and remove write nothing", async () => {
	const track = new Track("test");
	const producer = new Producer(track, { codec: stringCodec });
	expect(producer.insert("video")).toBe(true);
	expect(producer.insert("video")).toBe(false); // already present
	expect(producer.remove("audio")).toBe(false); // never present
	producer.finish();

	expect(await structure(track)).toEqual([1]);
});

test("live consumer sees each change", async () => {
	const track = new Track("test");
	const producer = new Producer(track, { codec: stringCodec });
	const consumer = new Consumer(track, { codec: stringCodec });

	producer.insert("video");
	expect(await consumer.next()).toEqual(set("video"));

	producer.insert("audio");
	expect(await consumer.next()).toEqual(set("video", "audio"));

	producer.remove("video");
	expect(await consumer.next()).toEqual(set("audio"));
});

test("late joiner reconstructs from deltas", async () => {
	const track = new Track("test");
	const producer = new Producer(track, { codec: stringCodec });
	producer.insert("a");
	producer.insert("b");
	producer.insert("c");
	producer.remove("a");
	producer.finish();

	expect((await drain(track)).at(-1)).toEqual(set("b", "c"));
});

test("frame cap rolls snapshot", async () => {
	const track = new Track("test");
	const producer = new Producer(track, { codec: stringCodec, deltaRatio: 1_000_000 });
	// Snapshot (frame 0) plus deltas fill the group until the frame cap forces a roll.
	for (let i = 0; i <= 256; i++) producer.insert(`item-${i}`);
	producer.finish();

	expect(await structure(track)).toEqual([256, 1]);
});
