import { expect, test } from "bun:test";
import { DEFAULT_MAX_FRAME_SIZE } from "@moq/flate";
import { Time, Track } from "@moq/net";
import { Consumer, Producer, Rolled } from "./index.ts";

// Ask for a replay window, so the superseded first group is delivered rather than skipped by the
// subscriber's default max-age budget. A rolled log is exactly the case where both groups matter.
const REPLAY_LATENCY = 30_000;

const payloads = (count: number) => Array.from({ length: count }, (_, n) => new Uint8Array(8).fill(n));

// Drain every payload currently available from a fresh consumer over the (finished) track.
async function drain(track: Track.Subscriber, compression: boolean): Promise<Uint8Array[]> {
	const consumer = new Consumer(track, { compression });
	const out: Uint8Array[] = [];
	for (;;) {
		const value = await consumer.next();
		if (value === undefined) break;
		out.push(value);
	}
	return out;
}

test("every payload survives in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track);
	const expected = payloads(5);
	for (const payload of expected) producer.append(payload);
	producer.finish();

	expect(await drain(track.subscribe(), false)).toEqual(expected);
});

test("compressed roundtrip in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	const expected = payloads(20);
	for (const payload of expected) producer.append(payload);
	producer.finish();

	expect(await drain(track.subscribe(), true)).toEqual(expected);
});

test("the whole log rides one group, never rolled", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	for (const payload of payloads(50)) producer.append(payload);
	producer.finish();

	const subscriber = track.subscribe().ordered();
	expect((await subscriber.nextGroup())?.sequence).toBe(0);
	expect(await subscriber.nextGroup()).toBeUndefined();
});

test("the shared window shrinks repetitive payloads", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(16));
	for (let n = 0; n < 8; n++) producer.append(payload);
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

	const consumer = new Consumer(track.subscribe({ maxAge: REPLAY_LATENCY }));
	expect(await consumer.next()).toBeDefined();
	expect(await consumer.next()).toBeDefined();
	await expect(consumer.next()).rejects.toThrow(Rolled);
});

test("an undecodable payload ends the log for a reader already inside the group", async () => {
	// The decoder-limit check ends the track like any other lost record, and by then an earlier
	// append may already have opened the group. A reader that pulled that group has to see the
	// failure rather than park on a group nothing will ever finish.
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	producer.append(payloads(1)[0]);

	const consumer = new Consumer(track.subscribe(), { compression: true });
	expect(await consumer.next()).toBeDefined();

	const oversized = new Uint8Array(DEFAULT_MAX_FRAME_SIZE + 1);
	expect(() => producer.append(oversized)).toThrow("limit");

	// Surfaces the terminal error rather than hanging on the still-open group.
	await expect(consumer.next()).rejects.toThrow("limit");
});
