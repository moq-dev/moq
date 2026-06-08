import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

// Reconstruct every value a consumer yields, in order.
async function drain(track: Track): Promise<Value[]> {
	const out: Value[] = [];
	for await (const value of new Consumer<Value>(track)) out.push(value);
	return out;
}

// Inspect the published layout via the public API: the frame count of each group, in order.
// The track must be finished first so group/frame reads terminate.
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

test("deltas off: a snapshot group per change", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track);
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// Two changes => two single-frame snapshot groups, reconstructed in order.
	expect(await drain(track)).toEqual([{ a: 1 }, { a: 2 }]);
});

test("live consumer sees each update", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track);
	const consumer = new Consumer<Value>(track);

	for (let n = 1; n <= 3; n++) {
		producer.update({ a: n });
		expect(await consumer.next()).toEqual({ a: n });
	}
});

test("unchanged value writes nothing", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track);
	producer.update({ a: 1 });
	producer.update({ a: 1 });
	producer.finish();

	expect(await structure(track)).toEqual([1]);
});

test("deltas share one group", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 1, b: 3 });
	producer.finish();

	// All updates fit in a single group as snapshot + two deltas.
	expect(await structure(track)).toEqual([3]);
});

test("deltas reconstruct to the final value", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	expect((await drain(track)).at(-1)).toEqual({ a: 5, b: 2 });
});

test("tight ratio rolls snapshots", async () => {
	const track = new Track("test");
	// A ratio of 1.0 leaves no room for any delta past the snapshot, so every change rolls.
	const producer = new Producer<Value>(track, { deltaRatio: 1.0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.update({ a: 3 });
	producer.finish();

	expect(await structure(track)).toEqual([1, 1, 1]);
});

test("array change is a wholesale delta", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100 });
	producer.update({ list: [1, 2] });
	producer.update({ list: [1, 2, 3] });
	producer.finish();

	// The array is replaced wholesale in a delta, so it stays in the same group.
	expect(await structure(track)).toEqual([2]);
});

test("frame cap rolls snapshot", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 1_000_000 });
	// First update is the snapshot; deltas fill the group until the frame cap forces a roll.
	for (let i = 0; i <= 256; i++) {
		producer.update({ n: i });
	}
	producer.finish();

	expect(await structure(track)).toEqual([256, 1]);
});
