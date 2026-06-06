import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

function groups(track: Track) {
	return track.state.groups.peek();
}

async function drain(consumer: Consumer<Value>): Promise<Value[]> {
	const out: Value[] = [];
	for await (const value of consumer) out.push(value);
	return out;
}

test("deltas off: snapshot per group, latest only", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track);
	producer.update({ a: 1 });
	producer.update({ a: 2 });

	// Two updates => two groups, each a full snapshot.
	expect(groups(track).length).toBe(2);

	producer.finish();
	// A consumer that joins after both exist only sees the latest.
	expect(await drain(new Consumer<Value>(track))).toEqual([{ a: 2 }]);
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

	expect(groups(track).length).toBe(1);
});

test("deltas share one group", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { maxDeltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 1, b: 3 });

	// All updates fit in a single group as snapshot + deltas.
	expect(groups(track).length).toBe(1);
	expect(groups(track)[0].state.frames.peek().length).toBe(3);

	producer.finish();
	expect((await drain(new Consumer<Value>(track))).at(-1)).toEqual({ a: 1, b: 3 });
});

test("tight ratio rolls snapshots", async () => {
	const track = new Track("test");
	// A ratio of 1.0 leaves no room for any delta past the snapshot, so every change rolls.
	const producer = new Producer<Value>(track, { maxDeltaRatio: 1.0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.update({ a: 3 });

	expect(groups(track).length).toBe(3);
});

test("array change is a wholesale delta", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { maxDeltaRatio: 100 });
	producer.update({ list: [1, 2] });
	producer.update({ list: [1, 2, 3] });

	// The array is replaced wholesale in a delta, so it stays in the same group.
	expect(groups(track).length).toBe(1);

	producer.finish();
	expect((await drain(new Consumer<Value>(track))).at(-1)).toEqual({ list: [1, 2, 3] });
});

test("frame cap rolls snapshot", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { maxDeltaRatio: 1_000_000 });
	// First update is the snapshot; then deltas fill the group until the frame cap forces a roll.
	for (let i = 0; i <= 256; i++) {
		producer.update({ n: i });
	}

	expect(groups(track).length).toBe(2);

	producer.finish();
	expect((await drain(new Consumer<Value>(track))).at(-1)).toEqual({ n: 256 });
});

test("late joiner reconstructs from deltas", async () => {
	const track = new Track("test");
	const producer = new Producer<Value>(track, { maxDeltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	expect((await drain(new Consumer<Value>(track))).at(-1)).toEqual({ a: 5, b: 2 });
});
