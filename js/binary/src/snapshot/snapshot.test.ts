import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer, Producer } from "./index.ts";

const bytes = (...values: number[]) => new Uint8Array(values);

// Drain every value currently available from a fresh consumer over the (finished) track.
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

test("one single-frame group per update", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track);
	producer.update(bytes(1));
	producer.update(bytes(2));
	producer.finish();

	// Two updates => two self-contained groups, so a consumer never needs an older one. The
	// in-memory track buffers both, so this drain sees each; over the wire a late joiner starts at
	// the newest group.
	expect(await drain(track.subscribe(), false)).toEqual([bytes(1), bytes(2)]);

	const subscriber = track.subscribe();
	const counts: number[] = [];
	for (;;) {
		const group = await subscriber.nextGroup();
		if (!group) break;
		let frames = 0;
		while ((await group.readFrame()) !== undefined) frames++;
		counts.push(frames);
	}
	expect(counts).toEqual([1, 1]);
});

test("a live consumer sees each update", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track.subscribe());

	for (let n = 0; n < 3; n++) {
		producer.update(bytes(n));
		expect(await consumer.next()).toEqual(bytes(n));
	}
	producer.finish();
});

test("compressed roundtrip", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(64));
	producer.update(payload);
	producer.finish();

	expect(await drain(track.subscribe(), true)).toEqual([payload]);
});

test("compression shrinks the frame on the wire", async () => {
	// A consumer that ignored the catalog's compression flag would read this raw and get garbage,
	// which is why the flag has to be carried rather than guessed.
	const track = new Track.Producer("test");
	const producer = new Producer(track, { compression: true });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(64));
	producer.update(payload);
	producer.finish();

	const group = await track.subscribe().nextGroup();
	const frame = await group?.readFrame();
	expect(frame?.payload.byteLength).toBeLessThan(payload.byteLength / 4);
});

test("a finished track ends the consumer", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer(track);
	producer.update(bytes(7));
	producer.finish();

	const consumer = new Consumer(track.subscribe());
	expect(await consumer.next()).toEqual(bytes(7));
	expect(await consumer.next()).toBeUndefined();
});
