import { expect, setSystemTime, test } from "bun:test";
import { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { CacheFull, MAX_GROUP_FRAMES } from "./group.ts";
import { Timestamp } from "./time.ts";
import type { Request as TrackRequest } from "./track.ts";
import { Producer as TrackProducer } from "./track.ts";

// The public API mints consumers internally (Producer.consume, the wire layers); tests act
// as a wire layer by subclassing, the same way lite's ConsumeBroadcast does.
class TestConsumer extends BroadcastConsumer {
	// biome-ignore lint/complexity/noUselessConstructor: widens the protected base constructor to public
	constructor() {
		super();
	}
}

// Observe whether an on-demand track request is pending without blocking: returns the
// next request if one has already been emitted, or undefined if none is (yet) waiting.
async function pendingRequest(broadcast: Pick<BroadcastProducer, "requested">): Promise<TrackRequest | undefined> {
	const none = Symbol("none");
	const result = await Promise.race([broadcast.requested(), Promise.resolve(none)]);
	return result === none ? undefined : (result as TrackRequest | undefined);
}

test("consumer dedupes repeat subscriptions onto one upstream request", async () => {
	const consumer = new TestConsumer();

	// Two subscriptions to the same track share one upstream subscription...
	const a = consumer.track("video").subscribe();
	const b = consumer.subscribe("video");

	const request = await pendingRequest(consumer);
	expect(request?.name).toBe("video");
	// ...so only one on-demand request is emitted for it.
	expect(await pendingRequest(consumer)).toBeUndefined();

	// Serving that single request fans out to both subscribers.
	if (!request) throw new Error("expected request");
	const producer = request.accept();
	producer.writeString("hello");
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");

	// A different track still opens its own request.
	consumer.subscribe("audio");
	expect((await pendingRequest(consumer))?.name).toBe("audio");

	// Once the shared track closes, a later subscribe re-opens it.
	producer.close();
	consumer.subscribe("video");
	expect((await pendingRequest(consumer))?.name).toBe("video");
});

test("a consumer clone shares the broadcast until every handle closes", () => {
	const consumer = new TestConsumer();
	const clone = consumer.clone();

	// Closing one handle leaves the shared broadcast live for the other...
	consumer.close();
	expect(consumer.closed.peek()).toBeUndefined();
	expect(clone.closed.peek()).toBeUndefined();

	// ...and closing the last handle closes it for both.
	clone.close();
	expect(consumer.closed.peek()).toBeDefined();
	expect(clone.closed.peek()).toBeDefined();
});

test("double-closing a consumer handle does not prematurely close the broadcast", () => {
	const consumer = new TestConsumer();
	const clone = consumer.clone();

	// The double close must decrement the shared count only once...
	clone.close();
	clone.close();
	expect(consumer.closed.peek()).toBeUndefined();

	// ...so the broadcast still stays live until the other handle closes.
	consumer.close();
	expect(consumer.closed.peek()).toBeDefined();
});

test("consumer track subscriptions fan out and close independently", async () => {
	const consumer = new TestConsumer();

	// Two subscriptions to one track dedupe onto a single upstream request...
	const a = consumer.subscribe("video");
	const b = consumer.subscribe("video");

	const request = await pendingRequest(consumer);
	if (!request) throw new Error("expected request");
	expect(await pendingRequest(consumer)).toBeUndefined();

	const producer = request.accept();
	producer.writeString("one");
	expect(await a.readString()).toBe("one");
	expect(await b.readString()).toBe("one");

	// ...and closing one subscriber leaves the other (and the shared upstream) delivering.
	a.close();
	producer.writeString("two");
	expect(await b.readString()).toBe("two");
});

test("subscribe serves a statically inserted track without a request", async () => {
	const broadcast = new BroadcastProducer();

	const track1 = new TrackProducer("track1").accept();
	broadcast.insertTrack(track1);
	track1.appendGroup().close();

	// The track already exists, so subscribe resolves immediately (no requested()).
	const sub1 = broadcast.track("track1").subscribe();
	expect((await sub1.nextGroup())?.sequence).toBe(0);

	// No on-demand request was emitted for it.
	expect(await pendingRequest(broadcast)).toBeUndefined();

	// A second static track behaves the same.
	const track2 = new TrackProducer("track2").accept();
	broadcast.insertTrack(track2);

	const sub2 = broadcast.track("track2").subscribe();
	track2.appendGroup().close();
	expect((await sub2.nextGroup())?.sequence).toBe(0);
});

test("two subscribers to one inserted track each get a full copy", async () => {
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");

	const a = broadcast.track("video").subscribe();
	const b = broadcast.track("video").subscribe();

	producer.writeString("hello");
	producer.writeString("world");

	// Neither subscriber steals from the other: both see every frame in order.
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");
	expect(await a.readString()).toBe("world");
	expect(await b.readString()).toBe("world");
});

test("a late subscriber replays the cached window", async () => {
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");

	// Written before anyone subscribes; retained in the cache for replay.
	producer.writeString("early");

	const late = broadcast.track("video").subscribe();
	expect(await late.readString()).toBe("early");

	producer.writeString("later");
	expect(await late.readString()).toBe("later");
});

test("a read throws CacheFull on a gap, then resyncs to the next group", async () => {
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");
	const sub = broadcast.track("video").subscribe();

	// Group 0 overflows its frame cap without being read, evicting the front: a gap.
	const g0 = producer.appendGroup();
	for (let i = 0; i < MAX_GROUP_FRAMES + 10; i++)
		g0.writeFrame({ payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	g0.close();

	// Group 1 is clean.
	const g1 = producer.appendGroup();
	g1.writeFrame({ payload: new TextEncoder().encode("ok"), timestamp: Timestamp.now() });
	g1.close();

	// The reader hits the gap in group 0 (error, not a silent skip), then the next
	// read resyncs from group 1.
	expect(sub.readFrame()).rejects.toBeInstanceOf(CacheFull);
	expect(await sub.readString()).toBe("ok");
});

test("a stalled consumer does not pin evicted groups", async () => {
	try {
		setSystemTime(new Date(10_000));

		const broadcast = new BroadcastProducer();
		const producer = broadcast.createTrack("video", { latencyMax: 1000 });

		// A subscriber that never reads. Its sink must not grow without bound.
		const stalled = broadcast.track("video").subscribe();

		producer.writeString("old");

		// Advance past the cache window and write again to trigger a prune. The old
		// (closed, aged-out) group is dropped from the stalled sink, not retained.
		setSystemTime(new Date(12_000));
		producer.writeString("fresh");

		// The next group in arrival order is the fresh one (seq 1): the old (seq 0) group
		// was evicted, not still buffered ahead of it.
		expect((await stalled.recvGroup())?.sequence).toBe(1);

		// Nothing else is buffered: a second recvGroup stays pending (the track is open),
		// proving exactly one group remained in the sink rather than two.
		const pending = Symbol("pending");
		const next = await Promise.race([stalled.recvGroup(), Promise.resolve(pending)]);
		expect(next).toBe(pending);
	} finally {
		setSystemTime();
	}
});

test("createTrack commits info up front", async () => {
	const broadcast = new BroadcastProducer();

	const producer = broadcast.createTrack("video", { latencyMax: 2000, priority: 3 });
	expect(producer.name).toBe("video");

	const info = await broadcast.track("video").info();
	expect(info.latencyMax).toBe(2000);
	expect(info.priority).toBe(3);
});

test("insertTrack rejects a duplicate live name", () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("dup");
	expect(() => broadcast.insertTrack(new TrackProducer("dup").accept())).toThrow();
});

test("a closed track is evicted and re-subscribing falls through to a request", async () => {
	const broadcast = new BroadcastProducer();

	const track1 = broadcast.createTrack("track1");
	track1.close();

	// The stale entry is gone, so subscribe creates an on-demand request instead.
	const pending = broadcast.track("track1").subscribe();
	expect(pending).toBeDefined();

	// That on-demand request is now waiting to be answered.
	const request = await broadcast.requested();
	expect(request?.name).toBe("track1");
});

test("removeTrack drops the static entry", async () => {
	// While the static entry exists, subscribing takes the fast path: no on-demand request.
	const kept = new BroadcastProducer();
	kept.createTrack("track1");
	kept.track("track1").subscribe();
	expect(await pendingRequest(kept)).toBeUndefined();

	// With the entry removed, subscribing falls through to an on-demand request.
	const removed = new BroadcastProducer();
	removed.createTrack("track1");
	removed.removeTrack("track1");
	removed.track("track1").subscribe();
	expect((await pendingRequest(removed))?.name).toBe("track1");
});

test("close rejects a still-pending track request so its subscriber unblocks", async () => {
	const broadcast = new TestConsumer();

	// Subscribing with no static entry queues an on-demand request; the subscriber
	// blocks on info() until that request is answered.
	const subscriber = broadcast.track("video").subscribe();
	const info = subscriber.info();

	// Closing must reject the still-queued request rather than silently dropping it,
	// so the awaiting subscriber rejects instead of hanging on a producer that will
	// never be served.
	broadcast.close();

	await expect(info).rejects.toThrow();
});
