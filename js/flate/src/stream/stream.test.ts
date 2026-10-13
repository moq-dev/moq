import { expect, spyOn, test } from "bun:test";
import { Group, Error as NetError, Time, Track } from "@moq/net";
import { DEFAULT_MAX_FRAME_SIZE } from "../codec.ts";
import { Consumer, Producer, Rolled } from "./index.ts";

// Ask for a replay window, so the superseded first group is delivered rather than skipped by the
// subscriber's default max delay budget. A rolled log is exactly the case where both groups matter.
const REPLAY_LATENCY = Time.Milli(30_000);

const payloads = (count: number) => Array.from({ length: count }, (_, n) => new Uint8Array(8).fill(n));

// Drain every payload currently available from a fresh consumer over the (finished) track.
async function drain(track: Track.Subscriber, compression: boolean): Promise<Uint8Array[]> {
	const consumer = new Consumer({ track, compression: compression ? "deflate" : "none" });
	const out: Uint8Array[] = [];
	for await (const { value } of consumer) out.push(value);
	return out;
}

test("every payload survives in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	const expected = payloads(5);
	for (const payload of expected) producer.append({ value: payload });
	producer.finish();

	expect(await drain(track.subscribe(), false)).toEqual(expected);
});

test("compressed roundtrip in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	const expected = payloads(20);
	for (const payload of expected) producer.append({ value: payload });
	producer.finish();

	expect(await drain(track.subscribe(), true)).toEqual(expected);
});

test("the whole log rides one group, never rolled", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	for (const payload of payloads(50)) producer.append({ value: payload });
	producer.finish();

	const subscriber = track.subscribe().ordered();
	expect((await subscriber.nextGroup())?.sequence).toBe(0);
	expect(await subscriber.nextGroup()).toBeUndefined();
});

test("the shared window shrinks repetitive payloads", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(16));
	for (let n = 0; n < 8; n++) producer.append({ value: payload });
	producer.finish();

	const group = await track.subscribe().ordered().nextGroup();
	const sizes: number[] = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (frame === undefined) break;
		sizes.push(frame.payload.byteLength);
	}

	expect(sizes.length).toBe(8);
	expect(sizes[sizes.length - 1]).toBeLessThan(sizes[0] ?? 0);
});

test("a second group is a rolled log, not a continuation", async () => {
	// A stream is one group. A publisher that rolls lost whatever would have completed the first,
	// so the read reports that rather than handing back the remainder as a continuous log.
	// Written by hand because this producer never rolls.
	const track = new Track.Producer("test");
	for (const pair of [payloads(2), payloads(2)]) {
		const group = track.appendGroup();
		for (const payload of pair) group.writeFrame({ payload, timestamp: Time.Timestamp.now() });
		group.close();
	}
	track.close();

	const consumer = new Consumer({ track: track.subscribe({ maxDelay: REPLAY_LATENCY }) });
	expect(await consumer.next()).toBeDefined();
	expect(await consumer.next()).toBeDefined();
	await expect(consumer.next()).rejects.toThrow(Rolled);
});

test("a second concurrent read is refused rather than served the first one's group", async () => {
	// Both calls would await the same in-flight `recvGroup`, and the loser would take the winner's
	// group for a second one and fail a perfectly good log. Rust gets this from `&mut self`.
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	producer.append({ value: payloads(1)[0] });
	producer.finish();

	const consumer = new Consumer({ track: track.subscribe() });
	const first = consumer.next();
	expect(() => consumer.next()).toThrow("multiple calls to next not supported");
	expect((await first)?.value).toEqual(payloads(1)[0]);
});

test("a second group is reported while the first is still open", async () => {
	// A boundary-only check would never look at the track again while a group is open, so a
	// publisher that opens a second group and leaves the first one running parks the read forever
	// on a log that already lost payloads. The payloads already in hand are still delivered.
	const track = new Track.Producer("test");

	// Both groups stay open, the way a publisher writing to two at once leaves them.
	const first = track.appendGroup();
	first.writeFrame({ payload: payloads(1)[0], timestamp: Time.Timestamp.now() });
	const second = track.appendGroup();
	second.writeFrame({ payload: payloads(2)[1], timestamp: Time.Timestamp.now() });

	const consumer = new Consumer({ track: track.subscribe({ maxDelay: REPLAY_LATENCY }) });
	expect((await consumer.next())?.value).toEqual(payloads(1)[0]);
	await expect(consumer.next()).rejects.toThrow(Rolled);

	// Both mirrors are released. The read that lost the race would otherwise stay registered on the
	// first group, keeping this consumer's subscription reachable after the caller drops it.
	expect(first.demand().used.peek()).toBe(false);
	expect(second.demand().used.peek()).toBe(false);

	// Sticky: a later read must not report the rest of the first group as a whole log.
	first.writeFrame({ payload: payloads(3)[2], timestamp: Time.Timestamp.now() });
	await expect(consumer.next()).rejects.toThrow(Rolled);
});

test("a failed write ends the log for a reader already inside the group", async () => {
	// A reader that pulled the group holds its own handle, so it has to see the failure rather than
	// park on a group nothing will ever finish.
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	producer.append({ value: payloads(1)[0] });

	const consumer = new Consumer({ track: track.subscribe(), compression: "deflate" });
	expect(await consumer.next()).toBeDefined();

	// The budget check refuses anything the group would, so stub the write to stand in for any
	// rejection past it.
	const failure = new Error("write rejected");
	const write = spyOn(Group.Producer.prototype, "writeFrame").mockImplementation(() => {
		throw failure;
	});
	try {
		expect(() => producer.append({ value: payloads(2)[1] })).toThrow(failure);
	} finally {
		write.mockRestore();
	}

	// Surfaces the terminal error rather than hanging on the still-open group.
	await expect(consumer.next()).rejects.toThrow(failure);
	expect(() => producer.append({ value: payloads(3)[2] })).toThrow();
});

// An ended log reports why it ended, even for a payload past the budget: `GroupTooLarge` would tell
// the caller the log is still writable.
test("an ended log refuses with its own error", () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });

	const failure = new Error("write rejected");
	const write = spyOn(Group.Producer.prototype, "writeFrame").mockImplementation(() => {
		throw failure;
	});
	try {
		expect(() => producer.append({ value: payloads(1)[0] })).toThrow(failure);
	} finally {
		write.mockRestore();
	}

	expect(() => producer.append({ value: new Uint8Array(Group.MAX_GROUP_CACHE_BYTES + 1) })).toThrow(failure);
});

// A payload past the group budget is refused before anything is written, so the log carries on: the
// next payload lands and decodes, which with compression proves the window never moved. One past the
// decoder's cap is refused the same way rather than ending the track.
for (const compression of [false, true]) {
	test(`an oversized payload is refused and the log continues (compression=${compression})`, async () => {
		const track = new Track.Producer("test");
		const producer = new Producer({ track, compression: compression ? "deflate" : "none" });
		producer.append({ value: payloads(1)[0] });

		const huge = new Uint8Array(DEFAULT_MAX_FRAME_SIZE + 1);
		expect(() => producer.append({ value: huge.subarray(0, Group.MAX_GROUP_CACHE_BYTES) })).toThrow(
			NetError.GroupTooLarge,
		);
		expect(() => producer.append({ value: huge })).toThrow(NetError.GroupTooLarge);

		producer.append({ value: payloads(2)[1] });
		producer.finish();
		expect(await drain(track.subscribe(), compression)).toEqual(payloads(2));
	});
}

// The budget check stands in for the decoder's cap: a payload that fits is one every consumer can
// inflate.
test("the group budget is within the decoder's cap", () => {
	expect(Group.MAX_GROUP_CACHE_BYTES).toBeLessThanOrEqual(DEFAULT_MAX_FRAME_SIZE);
});

test("a refused first payload publishes nothing", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	expect(() => producer.append({ value: new Uint8Array(DEFAULT_MAX_FRAME_SIZE + 1) })).toThrow(
		NetError.GroupTooLarge,
	);
	producer.finish();

	expect(await track.subscribe().ordered().nextGroup()).toBeUndefined();
});

// The budget covers the whole log, so once its frames are spent every append is refused, and the log
// written so far still finishes cleanly and reads back whole.
for (const compression of [false, true]) {
	test(`a spent budget refuses every append (compression=${compression})`, async () => {
		const track = new Track.Producer("test");
		const producer = new Producer({ track, compression: compression ? "deflate" : "none" });
		const frame = (n: number) => new Uint8Array(new Uint32Array([n]).buffer);
		for (let n = 0; n < Group.MAX_GROUP_FRAMES; n++) producer.append({ value: frame(n) });

		expect(() => producer.append({ value: frame(-1) })).toThrow(NetError.GroupTooLarge);
		expect(() => producer.append({ value: frame(-2) })).toThrow(NetError.GroupTooLarge);
		producer.finish();

		const out = await drain(track.subscribe(), compression);
		expect(out.length).toBe(Group.MAX_GROUP_FRAMES);
		expect(out.at(-1)).toEqual(frame(Group.MAX_GROUP_FRAMES - 1));
	});
}

// Counts the reactions `run` attaches to promises still pending once it returns. A promise holds each
// reaction until it settles, so one left per iteration on a promise that outlives the loop is a leak.
// Recorded by hand: Bun's `mock.contexts` misses the engine's own calls from `Promise.race`.
async function pendingReactions(run: () => Promise<void>): Promise<number> {
	const reacted: Promise<unknown>[] = [];
	const then = Promise.prototype.then;
	const spy = spyOn(Promise.prototype, "then").mockImplementation(function (this: Promise<unknown>, ...args) {
		reacted.push(this);
		return then.apply(this, args);
	} as typeof then);
	try {
		await run();
	} finally {
		spy.mockRestore();
	}
	return reacted.filter((promise) => Bun.peek.status(promise) === "pending").length;
}

// A blocked read races the frame against the track's next group, which stays pending for the whole
// log. Racing it per payload must not leave a reaction behind on it each time.
test("blocked reads leave nothing behind on the pending group read", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	const subscriber = track.subscribe();
	const consumer = new Consumer({ track: subscriber });

	const reactions = await pendingReactions(async () => {
		for (let n = 0; n < 1000; n++) {
			const next = consumer.next();
			producer.append({ value: new Uint8Array([n & 0xff]) });
			expect((await next)?.value[0]).toBe(n & 0xff);
		}
	});
	expect(reactions).toBeLessThan(10);

	subscriber.close();
	producer.finish();
});

test("an untimed payload reads back untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer({ track });
	producer.append({ value: new Uint8Array([1]) });
	producer.finish();

	const consumer = new Consumer({ track: track.subscribe() });
	expect(await consumer.next()).toEqual({ value: new Uint8Array([1]), at: undefined });
	expect(await consumer.next()).toBeUndefined();
});

test("each payload reads back with its capture timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI });
	const producer = new Producer({ track, compression: "deflate" });
	producer.append({ value: new Uint8Array([1]), at: Time.Timestamp.fromMillis(1_000) });
	producer.append({ value: new Uint8Array([2]), at: Time.Timestamp.fromMillis(2_000) });
	producer.finish();

	const consumer = new Consumer({ track: track.subscribe(), compression: "deflate" });
	const read: [number, number | undefined][] = [];
	for await (const { value, at } of consumer) read.push([value[0], at?.as(Time.Timescale.MILLI)]);
	expect(read).toEqual([
		[1, 1_000],
		[2, 2_000],
	]);
});
