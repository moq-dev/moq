import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer, Producer } from "./stream.ts";

type Rec = { n: number };

// Drain every record currently available from a fresh consumer over the (finished) track.
async function drain(track: Track, compression: boolean): Promise<number[]> {
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
	const track = new Track("test");
	const producer = new Producer<Rec>(track);
	for (let n = 0; n < 5; n++) producer.append({ n });
	producer.finish();

	expect(await drain(track, false)).toEqual([0, 1, 2, 3, 4]);
});

test("compressed roundtrip in order", async () => {
	const track = new Track("test");
	const producer = new Producer<Rec>(track, { compression: true });
	for (let n = 0; n < 20; n++) producer.append({ n });
	producer.finish();

	expect(await drain(track, true)).toEqual(Array.from({ length: 20 }, (_, n) => n));
});

test("the whole log rides one group, never rolled", async () => {
	const track = new Track("test");
	const producer = new Producer<Rec>(track, { compression: true });
	for (let n = 0; n < 50; n++) producer.append({ n });
	producer.finish();

	// A single group holds everything, and the consumer reads it all in order.
	const group0 = await track.nextGroupOrdered();
	expect(group0?.sequence).toBe(0);
	const group1 = await track.nextGroupOrdered();
	expect(group1).toBeUndefined();
});

test("records with embedded newlines round-trip (JSON escapes the newline)", async () => {
	// Each record is its own frame (one JSON object), and JSON.stringify escapes control characters,
	// so a string value containing a newline round-trips cleanly.
	const track = new Track("test");
	const producer = new Producer<{ s: string }>(track, { compression: true });
	const value = { s: "line1\nline2\ttab" };
	for (let i = 0; i < 4; i++) producer.append(value);
	producer.finish();

	const consumer = new Consumer<{ s: string }>(track, { compression: true });
	const out: { s: string }[] = [];
	for (;;) {
		const record = await consumer.next();
		if (record === undefined) break;
		out.push(record);
	}
	expect(out).toEqual([value, value, value, value]);
});
