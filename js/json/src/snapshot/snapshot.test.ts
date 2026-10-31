import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Group, Error as NetError, StreamCode, Time, Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Encoder } from "./encoder.ts";
import { Producer } from "./producer.ts";

type Value = Record<string, unknown>;

// These tests inspect complete finished timelines, so request a replay window
// instead of the transport's live-edge default.
const REPLAY_LATENCY = Milli(30_000);

// Reconstruct every state a consumer yields from `next()`, in order.
async function drain(track: Track.Subscriber): Promise<Value[]> {
	const out: Value[] = [];
	for await (const { value } of new Consumer<Value>({ track })) out.push(value);
	return out;
}

// Reconstruct every state a consumer yields from `latest()`, in order.
async function drainLatest(track: Track.Subscriber): Promise<Value[]> {
	const consumer = new Consumer<Value>({ track });
	const out: Value[] = [];
	for (let state = await consumer.latest(); state; state = await consumer.latest()) out.push(state.value);
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
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: 1 } });
	producer.update({ value: { a: 1, b: 2 } });
	producer.cut();
	producer.update({ value: { a: 1, b: 3 } });
	producer.finish();

	// The ratio would have kept every update in one group; the cut rolled it anyway. The replacement
	// opens with a full snapshot, so it is one frame rather than a delta appended to the first group.
	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([2, 1]);
	expect((await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }))).at(-1)).toEqual({ a: 1, b: 3 });
});

test("a cut republishes an unchanged value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1 } });
	producer.cut();

	// An unchanged value normally writes nothing. After a cut it must still open the replacement
	// group, or the value would only exist in a group no new consumer reads.
	producer.update({ value: { a: 1 } });
	producer.finish();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("a cut opens no replacement group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1 } });
	producer.cut();
	producer.finish();

	// Cutting closes the open group and stops there: no empty group for a consumer to advance into.
	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([1]);
});

test("a cut is idempotent", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });

	// Nothing published yet, so there is no group to cut.
	producer.cut();
	producer.cut();
	producer.update({ value: { a: 1 } });

	// And a repeated cut rolls once, not once per call.
	producer.cut();
	producer.cut();
	producer.update({ value: { a: 2 } });
	producer.finish();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("a cut is inert without deltas", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: false });
	producer.update({ value: { a: 1 } });

	// With deltas off every frame already closes its own group, so there is never one to cut.
	producer.cut();
	producer.update({ value: { a: 2 } });
	producer.finish();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([1, 1]);
});

test("deltas off: a snapshot group per change", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: false });
	producer.update({ value: { a: 1 } });
	producer.update({ value: { a: 2 } });
	producer.finish();

	// Two changes => two single-frame snapshot groups. `next()` reads both in order, while `latest()`
	// collapses the backlog to the newest value: older groups only hold superseded state (mirrors the
	// Rust consumer). The layout itself is asserted via structure() below.
	expect(await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }))).toEqual([{ a: 1 }, { a: 2 }]);
	expect(await drainLatest(track.subscribe({ maxDelay: REPLAY_LATENCY }))).toEqual([{ a: 2 }]);
});

// `latest()` holding a group must jump when a newer snapshot group rolls: the
// buffered deltas in the held group only reconstruct superseded state, and every
// group restarts from a full snapshot (mirrors the Rust consumer, which drains to
// the newest group on every poll).
test("latest abandons a held group when a newer snapshot group rolls", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	const consumer = new Consumer<Value>({ track: track.subscribe({ maxDelay: REPLAY_LATENCY }) });

	producer.update({ value: { a: 1 } });
	expect((await consumer.latest())?.value).toEqual({ a: 1 });

	// A delta lands in the held group, then a cut rolls a new snapshot group.
	producer.update({ value: { a: 2 } });
	producer.cut();
	producer.update({ value: { a: 3 } });
	producer.finish();

	expect((await consumer.latest())?.value).toEqual({ a: 3 });
	expect(await consumer.latest()).toBeUndefined();
});

// Skipping to the head never widens the reader's range: a capped reader reads the newest state
// below its cap, and a datagram past every group does not strand it.
test("latest stays inside the reader's range", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: false });
	const subscriber = track.subscribe({ maxDelay: REPLAY_LATENCY }).withRange({ end: { excluded: 2 } });
	const consumer = new Consumer<Value>({ track: subscriber });

	for (let n = 0; n < 4; n++) producer.update({ value: { a: n } });
	track.appendDatagram(Time.Timestamp.now(), new Uint8Array([0]));
	expect((await consumer.latest())?.value).toEqual({ a: 1 });
	producer.finish();
});

// `next()` finishes the held group before the newer one, so a playhead sees every state.
test("next reads a held group out before a newer snapshot group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	const consumer = new Consumer<Value>({ track: track.subscribe({ maxDelay: REPLAY_LATENCY }) });

	producer.update({ value: { a: 1 } });
	expect((await consumer.next())?.value).toEqual({ a: 1 });

	producer.update({ value: { a: 2 } });
	producer.cut();
	producer.update({ value: { a: 3 } });
	producer.finish();

	expect((await consumer.next())?.value).toEqual({ a: 2 });
	expect((await consumer.next())?.value).toEqual({ a: 3 });
	expect(await consumer.next()).toBeUndefined();
});

// Reading in order is still bounded: a group the subscription's max delay proves stale is skipped,
// as on any ordered track, so a reader that falls behind jumps ahead instead of replaying it.
test("next skips the groups the max delay abandons", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI });
	const producer = new Producer<Value>({ track, deltas: false });
	for (let n = 1; n <= 3; n++) producer.update({ value: { n }, at: Time.Timestamp.fromMillis(n * 1_000) });
	producer.finish();

	expect(await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }))).toEqual([{ n: 1 }, { n: 2 }, { n: 3 }]);
	expect(await drain(track.subscribe({ maxDelay: Milli(500) }))).toEqual([{ n: 2 }, { n: 3 }]);
	// The default of zero keeps only the newest group.
	expect(await drain(track.subscribe())).toEqual([{ n: 3 }]);
});

test("live consumer sees each update", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	const consumer = new Consumer<Value>({ track: track.subscribe() });

	for (let n = 1; n <= 3; n++) {
		producer.update({ value: { a: n } });
		expect((await consumer.next())?.value).toEqual({ a: n });
	}
});

test("unchanged value writes nothing", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	producer.update({ value: { a: 1 } });
	producer.update({ value: { a: 1 } });
	producer.finish();

	expect(await structure(track.subscribe().ordered())).toEqual([1]);
});

test("deltas share one group", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: 1 } });
	producer.update({ value: { a: 1, b: 2 } });
	producer.update({ value: { a: 1, b: 3 } });
	producer.finish();

	// All updates fit in a single group as snapshot + two deltas.
	expect(await structure(track.subscribe().ordered())).toEqual([3]);
});

test("deltas reconstruct to the final value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: 1 } });
	producer.update({ value: { a: 1, b: 2 } });
	producer.update({ value: { a: 5, b: 2 } });
	producer.finish();

	expect((await drain(track.subscribe())).at(-1)).toEqual({ a: 5, b: 2 });
});

// `mutate()` edits the shared document: multiple owners edit one producer, each touching its own
// keys, and each call publishes. This is how the catalog producer is extended (e.g. an scte35
// section) without a single owner having to rebuild the whole document.
test("mutate composes independent owners", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, initial: {} });
	const consumer = new Consumer<Value>({ track: track.subscribe() });

	producer.mutate((v) => {
		v.video = "v1";
	});
	expect((await consumer.next())?.value).toEqual({ video: "v1" });

	// A second owner starts from the latest value and adds its own key without clobbering the first.
	producer.mutate((v) => {
		v.scte35 = { id: 1 };
	});
	expect((await consumer.next())?.value).toEqual({ video: "v1", scte35: { id: 1 } });
});

test("mutate starts from the configured initial value", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, initial: {} });
	const consumer = new Consumer<Value>({ track: track.subscribe() });

	producer.mutate((v) => {
		v.a = 1;
	});
	expect((await consumer.next())?.value).toEqual({ a: 1 });
});

// A rejected edit never reached the wire, so the next owner starts from the last value that did.
// Carrying the rejected edit forward would fail every later publish the same way. (Mirrors the Rust
// `a_rejected_edit_leaves_modify_at_the_last_written_value`.)
test("a rejected mutate leaves the value at the last one written", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, initial: {} });
	producer.mutate((v) => {
		v.video = "v1";
	});

	// Serializes past the group cache limit, so the frame cannot be published.
	expect(() =>
		producer.mutate((v) => {
			v.big = "x".repeat(Group.MAX_GROUP_CACHE_BYTES + 1);
		}),
	).toThrow(NetError.FrameTooLarge);

	producer.mutate((v) => {
		v.scte35 = 42;
	});
	producer.finish();
	expect(await drainLatest(track.subscribe())).toEqual([{ video: "v1", scte35: 42 }]);
});

test("mutate without a prior value or initial throws", () => {
	const producer = new Producer<Value>({ track: new Track.Producer("test") });
	expect(() => producer.mutate(() => {})).toThrow();
});

// Removing a section drops it from the reconstructed value, so a consumer detects the removal.
// Exercised with deltas on to cover the merge-patch null-deletion path.
test("mutate removes a section", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true, initial: {} });
	const consumer = new Consumer<Value>({ track: track.subscribe() });

	producer.mutate((v) => {
		v.a = 1;
		v.scte35 = { id: 1 };
	});
	expect((await consumer.next())?.value).toEqual({ a: 1, scte35: { id: 1 } });

	producer.mutate((v) => {
		delete v.scte35;
	});
	expect((await consumer.next())?.value).toEqual({ a: 1 });
});

// Let the producer see a demand change, which the signal delivers in a microtask.
async function settle(): Promise<void> {
	await new Promise<void>((resolve) => queueMicrotask(resolve));
}

// Publish `{"n":0}` through `{"n":3}`: a 7-byte snapshot and 21 bytes of deltas.
function fill(producer: Producer<Value>): void {
	for (let n = 0; n <= 3; n++) producer.update({ value: { n } });
}

test("deltas never roll without a joiner", async () => {
	const track = new Track.Producer("test").accept({ refresh: 1 });
	const producer = new Producer<Value>({ track });
	for (let n = 0; n <= 10; n++) producer.update({ value: { n } });
	producer.finish();

	// The deltas outweigh the snapshot ten times over, but nobody joined to ask for a fresh group.
	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([11]);
});

test("a joiner over the ratio gets a fresh snapshot", async () => {
	for (const [refresh, layout] of [
		[8, [4]],
		[1, [4, 1]],
	] as const) {
		const track = new Track.Producer("test").accept({ refresh });
		const producer = new Producer<Value>({ track });
		fill(producer);

		// The joiner still reads the open group; the restatement serves whoever joins next.
		const joiner = track.subscribe();
		await settle();
		producer.finish();
		joiner.close();

		expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([...layout]);
		expect(await drainLatest(track.subscribe())).toEqual([{ n: 3 }]);
	}
});

test("a refresh reuses the newest timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, refresh: 1 });
	const producer = new Producer<Value>({ track });
	for (let n = 0; n <= 3; n++) producer.update({ value: { n }, at: Time.Timestamp.fromMillis(n * 10) });

	const joiner = track.subscribe();
	await settle();
	producer.finish();
	joiner.close();

	const ordered = track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered();
	const timestamps: (number | undefined)[] = [];
	for (let group = await ordered.nextGroup(); group; group = await ordered.nextGroup()) {
		const frame = await group.readFrame();
		timestamps.push(frame?.timestamp?.asMillis());
		while ((await group.readFrame()) !== undefined) {}
	}
	// The restated group opens at the newest timestamp written, not a new one.
	expect(timestamps).toEqual([0, 30]);
});

test("a refresh after an older update starts at the newest timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, refresh: 1 });
	const producer = new Producer<Value>({ track });
	// Deltas may carry timestamps older than their group's first frame.
	for (const [n, ms] of [30, 10, 20, 15].entries())
		producer.update({ value: { n }, at: Time.Timestamp.fromMillis(ms) });

	const joiner = track.subscribe();
	await settle();
	producer.finish();
	joiner.close();

	const ordered = track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered();
	const restated: [number | undefined, unknown][] = [];
	for (let group = await ordered.nextGroup(); group; group = await ordered.nextGroup()) {
		const frame = await group.readFrame();
		restated.push([frame?.timestamp?.asMillis(), frame && JSON.parse(new TextDecoder().decode(frame.payload))]);
		while ((await group.readFrame()) !== undefined) {}
	}
	// Restating at the last update's 15 would rewind past group 0's start and be refused.
	expect(restated).toEqual([
		[30, { n: 0 }],
		[30, { n: 3 }],
	]);
});

// A refresh restates the last value written, never one whose update was rejected, and the next
// update rides as a delta against it. (Mirrors the Rust
// `a_refresh_after_a_rejected_update_restates_the_last_written_value`.)
test("a refresh after a rejected update restates the last written value", async () => {
	const track = new Track.Producer("test").accept({ refresh: 1 });
	const producer = new Producer<Value>({ track });
	fill(producer);

	// Serializes past the group cache limit, so the frame cannot be published.
	const oversized = { big: "x".repeat(Group.MAX_GROUP_CACHE_BYTES + 1) };
	expect(() => producer.update({ value: oversized })).toThrow(NetError.FrameTooLarge);

	const joiner = track.subscribe();
	await settle();
	producer.update({ value: { n: 4 } });
	producer.finish();
	joiner.close();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([4, 2]);
	expect(await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }))).toEqual([
		{ n: 0 },
		{ n: 1 },
		{ n: 2 },
		{ n: 3 },
		{ n: 3 },
		{ n: 4 },
	]);
});

test("no ratio never refreshes", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track });
	fill(producer);

	const joiner = track.subscribe();
	await settle();
	producer.finish();
	joiner.close();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([4]);
});

test("a compressed refresh opens a fresh group", async () => {
	const track = new Track.Producer("test").accept({ refresh: 0, compress: true });
	const producer = new Producer<Value>({ track });
	producer.update({ value: { a: 1, b: 1 } });
	producer.update({ value: { a: 1, b: 2 } });

	const joiner = track.subscribe();
	await settle();
	// The change after the restatement rides as a delta in the new group.
	producer.update({ value: { a: 1, b: 3 } });
	producer.finish();
	joiner.close();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([2, 2]);
	const consumer = new Consumer<Value>({ track: track.subscribe() });
	const values: Value[] = [];
	for await (const { value } of consumer) values.push(value);
	expect(values).toEqual([
		{ a: 1, b: 2 },
		{ a: 1, b: 3 },
	]);
});

test("array change is a wholesale delta", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { list: [1, 2] } });
	producer.update({ value: { list: [1, 2, 3] } });
	producer.finish();

	// The array is replaced wholesale in a delta, so it stays in the same group.
	expect(await structure(track.subscribe().ordered())).toEqual([2]);
});

test("a late joiner reads a buffered backlog state by state, or collapses it to the latest", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	const subscriber = track.subscribe();
	const collapsed = track.subscribe();
	for (let n = 0; n <= 20; n++) {
		producer.update({ value: { n } });
	}
	producer.finish();

	// A whole group's worth of snapshot + deltas is buffered before the consumer reads. `next()`
	// yields every state in it; `latest()` applies them all but yields only the head, once.
	expect(await drain(subscriber)).toEqual(Array.from({ length: 21 }, (_, n) => ({ n })));
	expect(await drainLatest(collapsed)).toEqual([{ n: 20 }]);
});

test("frame cap rolls snapshot", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	// First update is the snapshot; deltas fill the group until the frame cap forces a roll.
	for (let i = 0; i <= 256; i++) {
		producer.update({ value: { n: i } });
	}
	producer.finish();

	expect(await structure(track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered())).toEqual([256, 1]);
});

test("a rejected update leaves the previous value readable", async () => {
	// A keyframe closes the previous group and publishes its replacement before writing, so
	// rejecting the frame inside writeFrame would supersede the last good value with an empty group.
	// A snapshot consumer jumps to the newest, so that value would vanish on a failed update.
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: false });
	producer.update({ value: { keep: true } });

	// Serializes past the group cache limit, so the frame cannot be published.
	const oversized = { big: "x".repeat(Group.MAX_GROUP_CACHE_BYTES + 1) };
	expect(() => producer.update({ value: oversized })).toThrow(NetError.FrameTooLarge);
	producer.finish();

	// A reader arriving now still finds the last good value, not an empty superseding group.
	expect(await drain(track.subscribe())).toEqual([{ keep: true }]);
});

test("a delta that would overflow the snapshot rolls a new one instead", async () => {
	// A delta is only readable while its group still holds the snapshot it applies to. A patch
	// that pushes the group past the budget would abort it, leaving a late subscriber with no
	// value. Mirrors the cumulative check in the Rust encoder.
	//
	// The value is replaced rather than grown, so each snapshot fits the budget on its own while the
	// snapshot plus the patch that rewrites it does not.
	const size = 40;
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, maxGroupBytes: 64 });

	producer.update({ value: { v: "x".repeat(size) } });
	producer.update({ value: { v: "y".repeat(size) } });
	producer.finish();

	// Two groups, a self-contained snapshot each, rather than one whose frame 0 was evicted.
	const subscriber = track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered();
	expect((await subscriber.nextGroup())?.sequence).toBe(0);
	expect((await subscriber.nextGroup())?.sequence).toBe(1);
	expect(await subscriber.nextGroup()).toBeUndefined();

	// The newest value is readable with no earlier frame to apply it to.
	const values = await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }));
	expect(values[values.length - 1]).toEqual({ v: "y".repeat(size) });
});

test("a snapshot past the budget is refused", () => {
	for (const deltas of [false, true]) {
		const encoder = new Encoder<Value>({ deltas, maxGroupBytes: 100 });
		expect(() => encoder.update({ v: "x".repeat(1000) })).toThrow(NetError.FrameTooLarge);
		expect(encoder.value).toBeUndefined();
	}
});

// A publisher gives up a group a reader is still on once a newer one supersedes it, which every
// moq-lite version reports as OLD. The reader skips to the newer snapshot instead of failing.
test("snapshot consumer skips a group the publisher dropped mid-read", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer<Value>({ track: track.subscribe() });
	const first = track.appendGroup();
	first.writeFrame({ payload: new TextEncoder().encode('{"v":0}'), timestamp: Time.Timestamp.now() });
	expect((await consumer.latest())?.value).toEqual({ v: 0 });

	const next = consumer.latest();
	first.abort(new NetError.Stream(StreamCode.Old));
	const second = track.appendGroup();
	second.writeFrame({ payload: new TextEncoder().encode('{"v":1}'), timestamp: Time.Timestamp.now() });
	expect((await next)?.value).toEqual({ v: 1 });
});

// A malformed or failed group must reach the caller; only an explicit retention gap is resumable.
test("snapshot consumer propagates a non-gap frame failure", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer<Value>({ track: track.subscribe() });
	const group = track.appendGroup();
	group.writeFrame({ payload: new TextEncoder().encode('{"ok":true}'), timestamp: Time.Timestamp.now() });
	expect((await consumer.next())?.value).toEqual({ ok: true });
	group.abort(new NetError.Stream(StreamCode.Internal));
	await expect(consumer.next()).rejects.toMatchObject({ code: StreamCode.Internal });
});

test("an update without a timestamp goes out untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: "x".repeat(64) } });
	producer.mutate((value) => {
		value.a = 2;
	});
	producer.finish();

	const group = await track.subscribe().ordered().nextGroup();
	expect((await group?.readFrame())?.timestamp).toBeUndefined();
	expect((await group?.readFrame())?.timestamp).toBeUndefined();
});

test("a capture timestamp is written on snapshots and deltas alike", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: "x".repeat(64) }, at: Time.Timestamp.fromMillis(1_000) });
	producer.update({ value: { a: 2, b: "x".repeat(64) }, at: Time.Timestamp.fromMillis(2_000) });
	producer.finish();

	const group = await track.subscribe().ordered().nextGroup();
	expect((await group?.readFrame())?.timestamp?.as(Time.Timescale.MILLI)).toBe(1_000);
	expect((await group?.readFrame())?.timestamp?.as(Time.Timescale.MILLI)).toBe(2_000);
});

test("an untimed state reads back untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer<Value>({ track, deltas: true });
	producer.update({ value: { a: 1, b: "x".repeat(64) } });
	producer.update({ value: { a: 2, b: "x".repeat(64) } });
	producer.finish();

	const consumer = new Consumer<Value>({ track: track.subscribe() });
	expect(await consumer.next()).toEqual({ value: { a: 1, b: "x".repeat(64) }, at: undefined });
	expect(await consumer.next()).toEqual({ value: { a: 2, b: "x".repeat(64) }, at: undefined });
});

// Each state carries the timestamp of the frame that produced it, snapshot or delta, so a caller can
// hold it back until a playhead reaches it. `latest()` reports the head's own frame.
test("each state reads back with its frame's timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, compress: true });
	const producer = new Producer<Value>({ track, deltas: true });
	for (let n = 1; n <= 3; n++) {
		producer.update({ value: { n, b: "x".repeat(64) }, at: Time.Timestamp.fromMillis(n * 1_000) });
	}
	producer.finish();

	const subscription = { maxDelay: REPLAY_LATENCY };
	const consumer = new Consumer<Value>({ track: track.subscribe(subscription) });
	const states: [unknown, number | undefined][] = [];
	for await (const { value, at } of consumer) states.push([value.n, at?.as(Time.Timescale.MILLI)]);
	expect(states).toEqual([
		[1, 1_000],
		[2, 2_000],
		[3, 3_000],
	]);

	const latest = new Consumer<Value>({ track: track.subscribe(subscription) });
	const head = await latest.latest();
	expect(head?.value.n).toBe(3);
	expect(head?.at?.as(Time.Timescale.MILLI)).toBe(3_000);
});
