import { expect, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { Consumer, Producer } from "./index.ts";

const bytes = (...values: number[]) => new Uint8Array(values);

// Walking a finished track's groups inspects a complete timeline, so request a replay window
// instead of the transport's live-edge default, which skips every superseded group.
const REPLAY_LATENCY = 30_000;

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

	// Two updates => two self-contained groups, so a consumer never needs an older one: a reader
	// that arrives after both discards the superseded first group instead of replaying it.
	expect(await drain(track.subscribe(), false)).toEqual([bytes(2)]);

	const subscriber = track.subscribe({ maxAge: REPLAY_LATENCY }).ordered();
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

	const group = await track.subscribe().ordered().nextGroup();
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

test("a backlog collapses to the newest value", async () => {
	// A consumer that fell behind (or joined late) must not replay every superseded value: its
	// latency would grow with the backlog, and each older group is already superseded by design.
	const track = new Track.Producer("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track.subscribe());

	for (let n = 0; n < 10; n++) producer.update(bytes(n));
	producer.finish();

	expect(await consumer.next()).toEqual(bytes(9));
	expect(await consumer.next()).toBeUndefined();
});

test("a newer group preempts an open one", async () => {
	// A group whose close is delayed must not park the reader while a newer value is available.
	// Snapshot mode exists to deliver the current value, not to wait out a stale group's FIN, and
	// groups ride independent QUIC streams so a newer one can land first.
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe());

	const stale = track.appendGroup();
	stale.writeFrame({ payload: bytes(1), timestamp: Time.Timestamp.now() });
	expect(await consumer.next()).toEqual(bytes(1));

	// A complete newer value arrives while the previous group is still open.
	const fresh = track.appendGroup();
	fresh.writeFrame({ payload: bytes(2), timestamp: Time.Timestamp.now() });
	fresh.close();

	expect(await consumer.next()).toEqual(bytes(2));

	stale.close();
	track.close();
});

test("an aborted track surfaces its error instead of spinning", async () => {
	// Every read after an abort throws the same terminal error, so swallowing it as if it were a
	// recoverable Lagged would loop forever on a rejected promise rather than telling the caller
	// the subscription died.
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe());

	const boom = new Error("subscription aborted");
	track.close(boom);

	await expect(consumer.next()).rejects.toThrow("subscription aborted");
});
