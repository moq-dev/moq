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
	const producer = new Producer<Value>(track, { deltaRatio: 0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// Two changes => two single-frame snapshot groups, reconstructed in order.
	expect(await drain(track)).toEqual([{ a: 1 }, { a: 2 }]);
});

test("deltaRatio 0 disables deltas, like off", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// `0` is treated as off, not a degenerate "enabled" value that keeps the group open: each change
	// is its own single-frame snapshot group.
	expect(await structure(track)).toEqual([1, 1]);
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

// `mutate()` edits the shared document: multiple owners edit one producer, each touching its own
// keys, and each call publishes. This is how the catalog producer is extended (e.g. an scte35
// section) without a single owner having to rebuild the whole document.
test("mutate composes independent owners", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { initial: {} });
	const consumer = new Consumer<Value>(track);

	producer.mutate((v) => {
		v.video = "v1";
	});
	expect(await consumer.next()).toEqual({ video: "v1" });

	// A second owner starts from the latest value and adds its own key without clobbering the first.
	producer.mutate((v) => {
		v.scte35 = { id: 1 };
	});
	expect(await consumer.next()).toEqual({ video: "v1", scte35: { id: 1 } });
});

test("mutate starts from the configured initial value", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { initial: {} });
	const consumer = new Consumer<Value>(track);

	producer.mutate((v) => {
		v.a = 1;
	});
	expect(await consumer.next()).toEqual({ a: 1 });
});

test("mutate without a prior value or initial throws", () => {
	const producer = new Producer<Value>(new Track("test"));
	expect(() => producer.mutate(() => {})).toThrow();
});

// Removing a section drops it from the reconstructed value, so a consumer detects the removal.
// Exercised with deltas on to cover the merge-patch null-deletion path.
test("mutate removes a section", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100, initial: {} });
	const consumer = new Consumer<Value>(track);

	producer.mutate((v) => {
		v.a = 1;
		v.scte35 = { id: 1 };
	});
	expect(await consumer.next()).toEqual({ a: 1, scte35: { id: 1 } });

	producer.mutate((v) => {
		delete v.scte35;
	});
	expect(await consumer.next()).toEqual({ a: 1 });
});

test("tight ratio rolls snapshots", async () => {
	const track = new Track("test");
	// A ratio of 1 admits deltas only up to the snapshot size: with equal 7-byte frames that is a
	// single delta per group, so it rolls every other update.
	const producer = new Producer<Value>(track, { deltaRatio: 1 });
	producer.update({ a: 1 }); // snapshot, group 0
	producer.update({ a: 2 }); // delta, group 0
	producer.update({ a: 3 }); // exceeds budget, rolls group 1
	producer.update({ a: 4 }); // delta, group 1
	producer.finish();

	expect(await structure(track)).toEqual([2, 2]);
});

test("deltas stay within ratio times snapshot", async () => {
	const track = new Track("test");
	// The budget covers only the deltas, not the snapshot frame, measured against the current
	// snapshot size. Single-digit values keep every frame at a constant 7 bytes (`{"n":N}`), so a
	// ratio of 8 admits 8 deltas (8x the snapshot) on top of the snapshot before the 9th rolls.
	const producer = new Producer<Value>(track, { deltaRatio: 8 });
	for (let n = 0; n <= 9; n++) producer.update({ n });
	producer.finish();

	expect(await structure(track)).toEqual([9, 1]);
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

test("late joiner collapses a buffered backlog to the latest value", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { deltaRatio: 100 });
	for (let n = 0; n <= 20; n++) {
		producer.update({ n });
	}
	producer.finish();

	// A whole group's worth of snapshot + deltas is buffered before the consumer reads, so it applies
	// them all but yields only the latest value once, not every superseded state.
	expect(await drain(track)).toEqual([{ n: 20 }]);
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
