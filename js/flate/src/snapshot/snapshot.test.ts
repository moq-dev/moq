import { expect, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { Consumer, Producer } from "./index.ts";

const bytes = (...values: number[]) => new Uint8Array(values);

// Walking a finished track's groups inspects a complete timeline, so request a replay window
// instead of the transport's live-edge default, which skips every superseded group.
const REPLAY_LATENCY = Time.Milli(30_000);

// Drain every value `next()` yields from a fresh consumer over the (finished) track.
async function drain(track: Track.Subscriber, compression: boolean): Promise<Uint8Array[]> {
	const consumer = new Consumer({ track, compression: compression ? "deflate" : "none" });
	const out: Uint8Array[] = [];
	for await (const { value } of consumer) out.push(value);
	return out;
}

// Drain every value `latest()` yields from a fresh consumer over the (finished) track.
async function drainLatest(track: Track.Subscriber): Promise<Uint8Array[]> {
	const consumer = new Consumer({ track });
	const out: Uint8Array[] = [];
	for (let next = await consumer.latest(); next; next = await consumer.latest()) out.push(next.value);
	return out;
}

test("one single-frame group per update", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	producer.update({ value: bytes(1) });
	producer.update({ value: bytes(2) });
	producer.finish();

	// Two updates => two self-contained groups, so a consumer never needs an older one: `next()` reads
	// both in order, while `latest()` discards the superseded first group instead of replaying it.
	expect(await drain(track.subscribe({ maxDelay: REPLAY_LATENCY }), false)).toEqual([bytes(1), bytes(2)]);
	expect(await drainLatest(track.subscribe({ maxDelay: REPLAY_LATENCY }))).toEqual([bytes(2)]);

	const subscriber = track.subscribe({ maxDelay: REPLAY_LATENCY }).ordered();
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
	const producer = new Producer({ track });
	const consumer = new Consumer({ track: track.subscribe() });

	for (let n = 0; n < 3; n++) {
		producer.update({ value: bytes(n) });
		expect((await consumer.next())?.value).toEqual(bytes(n));
	}
	producer.finish();
});

test("compressed roundtrip", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(64));
	producer.update({ value: payload });
	producer.finish();

	expect(await drain(track.subscribe(), true)).toEqual([payload]);
});

test("compression shrinks the frame on the wire", async () => {
	// A consumer that ignored the catalog's compression flag would read this raw and get garbage,
	// which is why the flag has to be carried rather than guessed.
	const track = new Track.Producer("test");
	const producer = new Producer({ track, compression: "deflate" });
	const payload = new TextEncoder().encode("the quick brown fox".repeat(64));
	producer.update({ value: payload });
	producer.finish();

	const group = await track.subscribe().ordered().nextGroup();
	const frame = await group?.readFrame();
	expect(frame?.payload.byteLength).toBeLessThan(payload.byteLength / 4);
});

test("a finished track ends the consumer", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	producer.update({ value: bytes(7) });
	producer.finish();

	const consumer = new Consumer({ track: track.subscribe() });
	expect((await consumer.next())?.value).toEqual(bytes(7));
	expect(await consumer.next()).toBeUndefined();
});

test("latest collapses a backlog to the newest value", async () => {
	// A reader that fell behind (or joined late) and only wants the current value must not replay
	// every superseded one: its latency would grow with the backlog.
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	const consumer = new Consumer({ track: track.subscribe() });

	for (let n = 0; n < 10; n++) producer.update({ value: bytes(n) });
	producer.finish();

	expect((await consumer.latest())?.value).toEqual(bytes(9));
	expect(await consumer.latest()).toBeUndefined();
});

test("next reads a backlog in group order", async () => {
	// A reader syncing to a playhead needs every value it can still get, not just the head.
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	const consumer = new Consumer({ track: track.subscribe({ maxDelay: REPLAY_LATENCY }) });

	for (let n = 0; n < 10; n++) producer.update({ value: bytes(n) });
	producer.finish();

	for (let n = 0; n < 10; n++) expect((await consumer.next())?.value).toEqual(bytes(n));
	expect(await consumer.next()).toBeUndefined();
});

test("latest lets a newer group preempt an open one", async () => {
	// A group whose close is delayed must not park the reader while a newer value is available.
	// `latest()` exists to deliver the current value, not to wait out a stale group's FIN, and
	// groups ride independent QUIC streams so a newer one can land first.
	const track = new Track.Producer("test");
	const consumer = new Consumer({ track: track.subscribe() });

	const stale = track.appendGroup();
	stale.writeFrame({ payload: bytes(1), timestamp: Time.Timestamp.now() });
	expect((await consumer.latest())?.value).toEqual(bytes(1));

	// A complete newer value arrives while the previous group is still open.
	const fresh = track.appendGroup();
	fresh.writeFrame({ payload: bytes(2), timestamp: Time.Timestamp.now() });
	fresh.close();

	expect((await consumer.latest())?.value).toEqual(bytes(2));

	stale.close();
	track.close();
});

test("an aborted track surfaces its error instead of spinning", async () => {
	// Every read after an abort throws the same terminal error, so swallowing it as if it were a
	// recoverable Lagged would loop forever on a rejected promise rather than telling the caller
	// the subscription died.
	const track = new Track.Producer("test");
	const consumer = new Consumer({ track: track.subscribe() });

	const boom = new Error("subscription aborted");
	track.close(boom);

	await expect(consumer.next()).rejects.toThrow("subscription aborted");
});

test("a payload without a timestamp goes out untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer({ track });
	producer.update({ value: bytes(1) });
	producer.finish();

	const frame = await (await track.subscribe().ordered().nextGroup())?.readFrame();
	expect(frame?.timestamp).toBeUndefined();
});

test("a capture timestamp is written as the frame timestamp", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer({ track });
	const captured = Time.Timestamp.fromMillis(1_234);
	producer.update({ value: bytes(1), at: captured });
	producer.finish();

	const frame = await (await track.subscribe().ordered().nextGroup())?.readFrame();
	expect(frame?.timestamp?.as(Time.Timescale.MILLI)).toBe(1_234);
});

test("an untimed value reads back untimed", async () => {
	const track = new Track.Producer("test").accept({});
	const producer = new Producer({ track });
	producer.update({ value: bytes(1) });
	producer.finish();

	expect(await new Consumer({ track: track.subscribe() }).next()).toEqual({ value: bytes(1), at: undefined });
});

test("each value reads back with its frame's timestamp", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI });
	const producer = new Producer({ track, compression: "deflate" });
	for (let n = 1; n <= 3; n++) producer.update({ value: bytes(n), at: Time.Timestamp.fromMillis(n * 1_000) });
	producer.finish();

	const subscription = { maxDelay: REPLAY_LATENCY };
	const consumer = new Consumer({ track: track.subscribe(subscription), compression: "deflate" });
	const values: [number, number | undefined][] = [];
	for await (const { value, at } of consumer) values.push([value[0], at?.as(Time.Timescale.MILLI)]);
	expect(values).toEqual([
		[1, 1_000],
		[2, 2_000],
		[3, 3_000],
	]);

	const head = await new Consumer({ track: track.subscribe(subscription), compression: "deflate" }).latest();
	expect(head?.value).toEqual(bytes(3));
	expect(head?.at?.as(Time.Timescale.MILLI)).toBe(3_000);
});
