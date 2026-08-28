import { expect, test } from "bun:test";
import { Group, Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

// These tests inspect complete finished timelines, so request a replay window
// instead of the transport's live-edge default.
const REPLAY_LATENCY = 30_000;

// Reconstruct every value a consumer yields, in order.
async function drain(track: Track.Subscriber): Promise<Value[]> {
	const out: Value[] = [];
	for await (const value of new Consumer<Value>(track)) out.push(value);
	return out;
}

// Inspect the published layout via the public API: the frame count of each group, in order.
// The track must be finished first so group/frame reads terminate.
async function structure(track: Track.Ordered): Promise<number[]> {
	const counts: number[] = [];
	for (;;) {
		const group = await track.nextGroup();
		if (!group) break;

		let frames = 0;
		while ((await group.readFrame()) !== undefined) frames++;
		counts.push(frames);
	}
	return counts;
}

test("a cut makes the next update a snapshot group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.cut();
	producer.update({ a: 1, b: 3 });
	producer.finish();

	// The ratio would have kept every update in one group; the cut rolled it anyway. The replacement
	// opens with a full snapshot, so it is one frame rather than a delta appended to the first group.
	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([2, 1]);
	expect((await drain(track.subscribe({ maxAge: REPLAY_LATENCY }))).at(-1)).toEqual({ a: 1, b: 3 });
});

test("a cut republishes an unchanged value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ a: 1 });
	producer.cut();

	// An unchanged value normally writes nothing. After a cut it must still open the replacement
	// group, or the value would only exist in a group no new consumer reads.
	producer.update({ a: 1 });
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("a cut opens no replacement group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ a: 1 });
	producer.cut();
	producer.finish();

	// Cutting closes the open group and stops there: no empty group for a consumer to advance into.
	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([1]);
});

test("a cut is idempotent", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });

	// Nothing published yet, so there is no group to cut.
	producer.cut();
	producer.cut();
	producer.update({ a: 1 });

	// And a repeated cut rolls once, not once per call.
	producer.cut();
	producer.cut();
	producer.update({ a: 2 });
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("a cut is inert without deltas", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 0 });
	producer.update({ a: 1 });

	// With deltas off every frame already closes its own group, so there is never one to cut.
	producer.cut();
	producer.update({ a: 2 });
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("deltas off: a snapshot group per change", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// Two changes => two single-frame snapshot groups. A consumer joining after the fact
	// collapses the backlog to the newest value: older groups only hold superseded state
	// (mirrors the Rust consumer). The layout itself is asserted via structure() below.
	expect(await drain(track.subscribe({ maxAge: REPLAY_LATENCY }))).toEqual([{ a: 2 }]);
});

test("deltaRatio 0 disables deltas, like off", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 0 });
	producer.update({ a: 1 });
	producer.update({ a: 2 });
	producer.finish();

	// `0` is treated as off, not a degenerate "enabled" value that keeps the group open: each change
	// is its own single-frame snapshot group.
	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

// A consumer holding a group must jump when a newer snapshot group rolls: the
// buffered deltas in the held group only reconstruct superseded state, and every
// group restarts from a full snapshot (mirrors the Rust consumer, which drains to
// the newest group on every poll).
test("a held group is abandoned when a newer snapshot group rolls", async () => {
	const track = new Track.Producer("test");
	// A tiny ratio forces the update after any delta to roll a fresh snapshot group.
	const producer = new Producer<Value>({ track, deltaRatio: 0.001 });
	const consumer = new Consumer<Value>(track.subscribe({ maxAge: REPLAY_LATENCY }));

	producer.update({ a: 1 });
	expect(await consumer.next()).toEqual({ a: 1 });

	// A delta lands in the held group, then the ratio rolls a new snapshot group.
	producer.update({ a: 2 });
	producer.update({ a: 3 });
	producer.finish();

	expect(await consumer.next()).toEqual({ a: 3 });
});

test("live consumer sees each update", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	const consumer = new Consumer<Value>(track.subscribe());

	for (let n = 1; n <= 3; n++) {
		producer.update({ a: n });
		expect(await consumer.next()).toEqual({ a: n });
	}
});

test("unchanged value writes nothing", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	producer.update({ a: 1 });
	producer.update({ a: 1 });
	producer.finish();

	expect(await structure(track.subscribe().ordered())).toEqual([1]);
});

test("deltas share one group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 1, b: 3 });
	producer.finish();

	// All updates fit in a single group as snapshot + two deltas.
	expect(await structure(track.subscribe().ordered())).toEqual([3]);
});

test("deltas reconstruct to the final value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ a: 1, b: 1 });
	producer.update({ a: 1, b: 2 });
	producer.update({ a: 5, b: 2 });
	producer.finish();

	expect((await drain(track.subscribe())).at(-1)).toEqual({ a: 5, b: 2 });
});

// `mutate()` edits the shared document: multiple owners edit one producer, each touching its own
// keys, and each call publishes. This is how the catalog producer is extended (e.g. an scte35
// section) without a single owner having to rebuild the whole document.
test("mutate composes independent owners", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, initial: {} });
	const consumer = new Consumer<Value>(track.subscribe());

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
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, initial: {} });
	const consumer = new Consumer<Value>(track.subscribe());

	producer.mutate((v) => {
		v.a = 1;
	});
	expect(await consumer.next()).toEqual({ a: 1 });
});

test("mutate without a prior value or initial throws", () => {
	const producer = new Producer<Value>({ track: new Track.Producer("test") });
	expect(() => producer.mutate(() => {})).toThrow();
});

// Removing a section drops it from the reconstructed value, so a consumer detects the removal.
// Exercised with deltas on to cover the merge-patch null-deletion path.
test("mutate removes a section", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100, initial: {} });
	const consumer = new Consumer<Value>(track.subscribe());

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
	const track = new Track.Producer("test");
	// A ratio of 1 budgets deltas up to one snapshot (equal 7-byte frames => 7 bytes). The gate checks
	// the deltas already written, so the delta that tips the group over budget still lands (a one-frame
	// overshoot): group 0 takes two deltas (14 bytes) before the fourth update rolls group 1.
	const producer = new Producer<Value>({ track, deltaRatio: 1 });
	producer.update({ a: 1 }); // snapshot, group 0
	producer.update({ a: 2 }); // delta, group 0 (deltas = 7)
	producer.update({ a: 3 }); // delta, group 0 (deltas = 14, now over budget)
	producer.update({ a: 4 }); // budget already exceeded, rolls group 1
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([3, 1]);
});

test("deltas stay within ratio times snapshot", async () => {
	const track = new Track.Producer("test");
	// The budget covers only the deltas, not the snapshot frame, measured against the group's snapshot
	// size. Single-digit values keep every frame at a constant 7 bytes (`{"n":N}`), so a ratio of 8
	// budgets 56 bytes of deltas. The gate checks the deltas already written, so the group keeps filling
	// until they first exceed 56 (nine deltas = 63 bytes) and the next update rolls (a one-frame
	// overshoot past the 56-byte budget).
	const producer = new Producer<Value>({ track, deltaRatio: 8 });
	for (let n = 0; n <= 10; n++) producer.update({ n });
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([10, 1]);
});

test("array change is a wholesale delta", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	producer.update({ list: [1, 2] });
	producer.update({ list: [1, 2, 3] });
	producer.finish();

	// The array is replaced wholesale in a delta, so it stays in the same group.
	expect(await structure(track.subscribe().ordered())).toEqual([2]);
});

test("late joiner collapses a buffered backlog to the latest value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 100 });
	const subscriber = track.subscribe();
	for (let n = 0; n <= 20; n++) {
		producer.update({ n });
	}
	producer.finish();

	// A whole group's worth of snapshot + deltas is buffered before the consumer reads, so it applies
	// them all but yields only the latest value once, not every superseded state.
	expect(await drain(subscriber)).toEqual([{ n: 20 }]);
});

test("frame cap rolls snapshot", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 1_000_000 });
	// First update is the snapshot; deltas fill the group until the frame cap forces a roll.
	for (let i = 0; i <= 256; i++) {
		producer.update({ n: i });
	}
	producer.finish();

	expect(await structure(track.subscribe({ maxAge: REPLAY_LATENCY }).ordered())).toEqual([256, 1]);
});

test("a rejected update leaves the previous value readable", async () => {
	// A keyframe closes the previous group and publishes its replacement before writing, so
	// rejecting the frame inside writeFrame would supersede the last good value with an empty group.
	// A snapshot consumer jumps to the newest, so that value would vanish on a failed update.
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltaRatio: 0 });
	producer.update({ keep: true });

	// Serializes past the group cache limit, so the frame cannot be published.
	const oversized = { big: "x".repeat(Group.MAX_GROUP_CACHE_BYTES + 1) };
	expect(() => producer.update(oversized)).toThrow(Group.FrameTooLarge);
	producer.finish();

	// A reader arriving now still finds the last good value, not an empty superseding group.
	expect(await drain(track.subscribe())).toEqual([{ keep: true }]);
});

test("a delta that would evict the snapshot rolls a new one instead", async () => {
	// A delta is only readable while its group still holds the snapshot it applies to, and the group
	// cache evicts from the front. A patch that pushes the group past the cache would drop frame 0,
	// leaving a late subscriber with a base-less group instead of the current value. Mirrors the
	// cumulative check in the Rust encoder.
	//
	// The value is replaced rather than grown, so each snapshot fits the cache on its own while the
	// snapshot plus the patch that rewrites it does not.
	const half = Math.floor(Group.MAX_GROUP_CACHE_BYTES * 0.6);
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });

	producer.update({ v: "x".repeat(half) });
	producer.update({ v: "y".repeat(half) });
	producer.finish();

	// Two groups, a self-contained snapshot each, rather than one whose frame 0 was evicted.
	const subscriber = track.subscribe({ maxAge: REPLAY_LATENCY }).ordered();
	expect((await subscriber.nextGroup())?.sequence).toBe(0);
	expect((await subscriber.nextGroup())?.sequence).toBe(1);
	expect(await subscriber.nextGroup()).toBeUndefined();

	// The newest value is readable with no earlier frame to apply it to.
	const values = await drain(track.subscribe({ maxAge: REPLAY_LATENCY }));
	expect(values[values.length - 1]).toEqual({ v: "y".repeat(half) });
});

test("a compressed delta is gated on its encoded size, not its plaintext", async () => {
	// A sync-flushed DEFLATE frame can come out larger than its input, so the plaintext is not an
	// upper bound on what lands in the group. A snapshot that fills the cache to within a few bytes
	// plus a tiny patch that compresses to more than it measures would otherwise slip through the
	// gate and evict frame 0.
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, compression: true });

	// Highly repetitive, so the compressed snapshot lands just under the cap.
	producer.update({ v: "x".repeat(Group.MAX_GROUP_CACHE_BYTES) });
	producer.update({ v: "x".repeat(Group.MAX_GROUP_CACHE_BYTES), q: "a" });
	producer.finish();

	// Whatever the split, no group may exceed the cache, and the newest value must be readable.
	const subscriber = track.subscribe({ maxAge: REPLAY_LATENCY }).ordered();
	for (;;) {
		const group = await subscriber.nextGroup();
		if (!group) break;
		let bytes = 0;
		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;
			bytes += frame.payload.byteLength;
		}
		expect(bytes).toBeLessThanOrEqual(Group.MAX_GROUP_CACHE_BYTES);
	}

	const consumer = new Consumer<Value>(track.subscribe({ maxAge: REPLAY_LATENCY }), { compression: true });
	const values: Value[] = [];
	for await (const value of consumer) values.push(value);
	expect(values[values.length - 1]).toEqual({ v: "x".repeat(Group.MAX_GROUP_CACHE_BYTES), q: "a" });
});
