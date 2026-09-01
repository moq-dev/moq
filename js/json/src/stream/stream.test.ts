import { expect, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { Consumer, Producer, Rolled } from "./index.ts";

type Rec = { n: number };

// Drain every record currently available from a fresh consumer over the (finished) track.
async function drain(track: Track.Subscriber, compression: boolean): Promise<number[]> {
	const consumer = new Consumer<Rec>(track, { compression });
	const out: number[] = [];
	for (;;) {
		const record = await consumer.next();
		if (record === undefined) break;
		out.push(record.n);
	}
	return out;
}

test("plaintext roundtrip in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track);
	for (let n = 0; n < 5; n++) producer.append({ n });
	producer.finish();

	expect(await drain(track.subscribe(), false)).toEqual([0, 1, 2, 3, 4]);
});

test("compressed roundtrip in order", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track, { compression: true });
	for (let n = 0; n < 20; n++) producer.append({ n });
	producer.finish();

	expect(await drain(track.subscribe(), true)).toEqual(Array.from({ length: 20 }, (_, n) => n));
});

test("the whole log rides one group, never rolled", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track, { compression: true });
	for (let n = 0; n < 50; n++) producer.append({ n });
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
	const track = new Track.Producer("test");
	const producer = new Producer<{ s: string }>(track, { compression: true });
	const value = { s: "line1\nline2\ttab" };
	for (let i = 0; i < 4; i++) producer.append(value);
	producer.finish();

	const consumer = new Consumer<{ s: string }>(track.subscribe(), { compression: true });
	const out: { s: string }[] = [];
	for (;;) {
		const record = await consumer.next();
		if (record === undefined) break;
		out.push(record);
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
	// subscriber's default max-age budget once a newer group exists.
	const consumer = new Consumer<Rec>(track.subscribe({ maxAge: 30_000 }));
	expect(await consumer.next()).toEqual({ n: 0 });
	await expect(consumer.next()).rejects.toThrow(Rolled);

	// Sticky: a later read must not report the rest of the first group as a whole log.
	first.writeFrame({ payload: encode({ n: 2 }), timestamp: Time.Timestamp.now() });
	await expect(consumer.next()).rejects.toThrow(Rolled);
});
