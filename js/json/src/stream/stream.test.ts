import { expect, spyOn, test } from "bun:test";
import { Milli } from "@moq/clock";
import { StreamCode, Time, Track } from "@moq/net";
import * as z from "zod/mini";
import { Consumer, Producer, Rolled } from "./index.ts";

type Rec = { n: number };

// Drain every record currently available from a fresh consumer over the (finished) track.
async function drain(track: Track.Subscriber): Promise<number[]> {
	const consumer = new Consumer<Rec>({ track });
	const out: number[] = [];
	for (;;) {
		const record = await consumer.next();
		if (record === undefined) break;
		out.push(record.value.n);
	}
	return out;
}

test("plaintext roundtrip in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>({ track });
	for (let n = 0; n < 5; n++) producer.append({ value: { n } });
	producer.finish();

	expect(await drain(track.subscribe())).toEqual([0, 1, 2, 3, 4]);
});

test("compressed roundtrip in order", async () => {
	const track = new Track.Producer("test").accept({ compress: true });
	const producer = new Producer<Rec>({ track });
	for (let n = 0; n < 20; n++) producer.append({ value: { n } });
	producer.finish();

	expect(await drain(track.subscribe())).toEqual(Array.from({ length: 20 }, (_, n) => n));
});

test("a record whose worst-case slice outgrows the frame cap is refused, leaving the track open", async () => {
	const track = new Track.Producer("test").accept({ compress: true });
	const producer = new Producer<string>({ track });
	// 32 MiB of JSON fits the plain cap, but its DEFLATE bound does not.
	expect(() => producer.append({ value: "x".repeat(32 * 1024 * 1024 - 2) })).toThrow(
		expect.objectContaining({ code: StreamCode.FrameTooLarge }),
	);
	producer.append({ value: "small" });
	producer.finish();

	const consumer = new Consumer<string>({ track: track.subscribe() });
	expect((await consumer.next())?.value).toBe("small");
});

test("the whole log rides one group, never rolled", async () => {
	const track = new Track.Producer("test").accept({ compress: true });
	const producer = new Producer<Rec>({ track });
	for (let n = 0; n < 50; n++) producer.append({ value: { n } });
	producer.finish();

	// A single group holds everything, and the consumer reads it all in order.
	const subscriber = track.subscribe().ordered();
	const group0 = await subscriber.nextGroup();
	expect(group0?.sequence).toBe(0);
	const group1 = await subscriber.nextGroup();
	expect(group1).toBeUndefined();
});

test("records with embedded newlines round-trip (JSON escapes the newline)", async () => {
	// Each record is its own frame (one JSON object), and JSON.stringify escapes control characters,
	// so a string value containing a newline round-trips cleanly.
	const track = new Track.Producer("test").accept({ compress: true });
	const producer = new Producer<{ s: string }>({ track });
	const value = { s: "line1\nline2\ttab" };
	for (let i = 0; i < 4; i++) producer.append({ value: value });
	producer.finish();

	const consumer = new Consumer<{ s: string }>({ track: track.subscribe() });
	const out: { s: string }[] = [];
	for (;;) {
		const record = await consumer.next();
		if (record === undefined) break;
		out.push(record.value);
	}
	expect(out).toEqual([value, value, value, value]);
});

test("a second group is reported while the first is still open", async () => {
	// A stream is one group. A publisher that opens a second lost whatever would have completed the
	// first, so the read reports that rather than handing back the remainder as a continuous log.
	// A boundary-only check would never look at the track again while the first group is open, so
	// this parks forever without the eager check. Written by hand because this producer never rolls.
	const track = new Track.Producer("test");
	const encode = (record: Rec) => new TextEncoder().encode(JSON.stringify(record));

	// Both groups stay open, the way a publisher writing to two at once leaves them.
	const first = track.appendGroup();
	first.writeFrame({ payload: encode({ n: 0 }), timestamp: Time.Timestamp.now() });
	const second = track.appendGroup();
	second.writeFrame({ payload: encode({ n: 1 }), timestamp: Time.Timestamp.now() });

	// Ask for a replay window, so the first group is delivered rather than skipped by the
	// subscriber's default max delay budget once a newer group exists.
	const consumer = new Consumer<Rec>({ track: track.subscribe({ maxDelay: Milli(30_000) }) });
	expect((await consumer.next())?.value).toEqual({ n: 0 });
	await expect(consumer.next()).rejects.toBeInstanceOf(Rolled);

	// Both mirrors are released. The read that lost the race would otherwise stay registered on the
	// first group, keeping this consumer's subscription reachable after the caller drops it.
	expect(first.demand().wanted.peek()).toBe(false);
	expect(second.demand().wanted.peek()).toBe(false);

	// Sticky: a later read must not report the rest of the first group as a whole log.
	first.writeFrame({ payload: encode({ n: 2 }), timestamp: Time.Timestamp.now() });
	await expect(consumer.next()).rejects.toBeInstanceOf(Rolled);
});

for (const compress of [false, true]) {
	test(`schema validation stays inside the read (compress=${compress})`, async () => {
		const track = new Track.Producer("test").accept({ compress });
		const producer = new Producer<Rec>({ track });
		producer.append({ value: { n: 1 } });
		producer.finish();

		const schema = z.pipe(
			z.object({ n: z.number() }),
			z.transform((record) => {
				expect(() => consumer.next()).toThrow("multiple calls to next not supported");
				return record.n + 1;
			}),
		);
		const consumer = new Consumer({ track: track.subscribe(), schema });
		expect(await consumer.next()).toEqual({ value: 2, at: undefined });
		expect(await consumer.next()).toBeUndefined();
	});
}

test("a failed track preserves its error", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer({ track: track.subscribe() });
	const next = consumer.next();
	const failure = new Error("source failed");
	track.abort(failure);
	await expect(next).rejects.toBe(failure);
});

test("a second concurrent read is refused rather than served the first one's group", async () => {
	// Both calls would await the same in-flight `recvGroup`, and the loser would take the winner's
	// group for a second one and fail a perfectly good log. Rust gets this from `&mut self`.
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>({ track });
	producer.append({ value: { n: 0 } });
	producer.finish();

	const consumer = new Consumer<Rec>({ track: track.subscribe() });
	const first = consumer.next();
	expect(() => consumer.next()).toThrow("multiple calls to next not supported");
	expect((await first)?.value).toEqual({ n: 0 });
});

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
// log. Racing it per record must not leave a reaction behind on it each time.
test("blocked reads leave nothing behind on the pending group read", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>({ track });
	const subscriber = track.subscribe();
	const consumer = new Consumer<Rec>({ track: subscriber });

	const reactions = await pendingReactions(async () => {
		for (let n = 0; n < 1000; n++) {
			const next = consumer.next();
			producer.append({ value: { n } });
			expect((await next)?.value.n).toBe(n);
		}
	});
	expect(reactions).toBeLessThan(10);

	subscriber.close();
	producer.finish();
});

test("an untimed record reads back untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer<number>({ track });
	producer.append({ value: 1 });
	producer.finish();

	const consumer = new Consumer<number>({ track: track.subscribe() });
	expect(await consumer.next()).toEqual({ value: 1, at: undefined });
	expect(await consumer.next()).toBeUndefined();
});

test("each record reads back with its capture timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, compress: true });
	const producer = new Producer<number>({ track });
	producer.append({ value: 1, at: Time.Timestamp.fromMillis(1_000) });
	producer.append({ value: 2, at: Time.Timestamp.fromMillis(2_000) });
	producer.finish();

	const consumer = new Consumer<number>({ track: track.subscribe() });
	const records: [number, number | undefined][] = [];
	for await (const { value, at } of consumer) records.push([value, at?.as(Time.Timescale.MILLI)]);
	expect(records).toEqual([
		[1, 1_000],
		[2, 2_000],
	]);
});

for (const compress of [false, true]) {
	test(`a stream has no record ceiling (compress=${compress})`, async () => {
		const track = new Track.Producer("test").accept({ compress });
		const producer = new Producer<Rec>({ track });
		for (let n = 0; n < 9000; n++) producer.append({ value: { n } });
		producer.finish();
		const records = await drain(track.subscribe());
		expect(records.length).toBe(9000);
		expect(records.at(-1)).toBe(8999);
	});

	test(`a live stream passes several tails and refuses a truncated replay (compress=${compress})`, async () => {
		const track = new Track.Producer("test").accept({ compress });
		const producer = new Producer<{ n: number; pad: string }>({ track });
		const fast = new Consumer<{ n: number; pad: string }>({ track: track.subscribe() });
		const slow = new Consumer<Rec>({ track: track.subscribe() });
		const pad = "x".repeat(1024 * 1024);
		for (let n = 0; n < 100; n++) {
			producer.append({ value: { n, pad } });
			expect((await fast.next())?.value.n).toBe(n);
		}
		await expect(slow.next()).rejects.toMatchObject({ code: StreamCode.Evicted });
		const late = new Consumer<Rec>({ track: track.subscribe() });
		await expect(late.next()).rejects.toMatchObject({ code: StreamCode.Evicted });
		producer.finish();
		expect(await fast.next()).toBeUndefined();
	});
}
