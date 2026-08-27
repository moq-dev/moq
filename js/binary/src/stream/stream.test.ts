import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer, Producer } from "./index.ts";

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

	const subscriber = track.subscribe();
	expect((await subscriber.nextGroup())?.sequence).toBe(0);
	expect(await subscriber.nextGroup()).toBeUndefined();
});

test("the shared window shrinks repetitive payloads", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(16));
	for (let n = 0; n < 8; n++) producer.append(payload);
	producer.finish();

	const group = await track.subscribe().nextGroup();
	const sizes: number[] = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (frame === undefined) break;
		sizes.push(frame.payload.byteLength);
	}

	expect(sizes.length).toBe(8);
	expect(sizes[sizes.length - 1]).toBeLessThan(sizes[0] ?? 0);
});
