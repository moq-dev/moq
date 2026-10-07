import { expect, setSystemTime, spyOn, test } from "bun:test";
import { Signal } from "@moq/signals";
import { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { GroupTooLarge, NotFound } from "./error.ts";
import { Producer as GroupProducer, MAX_GROUP_FRAMES } from "./group.ts";
import { Milli, Timescale, Timestamp } from "./time.ts";
import type { Request as TrackRequest } from "./track.ts";
import { Producer as TrackProducer } from "./track.ts";
import { wireOf } from "./wire.ts";

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
async function pendingRequest(broadcast: BroadcastProducer | BroadcastConsumer): Promise<TrackRequest | undefined> {
	const none = Symbol("none");
	const result = await Promise.race([wireOf(broadcast).requested(), Promise.resolve(none)]);
	return result === none ? undefined : (result as TrackRequest | undefined);
}

test("consumer dedupes repeat subscriptions onto one upstream request", async () => {
	const consumer = new TestConsumer();

	// Two subscriptions to the same track share one upstream subscription...
	const a = consumer.track("video").subscribe().ordered();
	const b = consumer.track("video").subscribe().ordered();

	const request = await pendingRequest(consumer);
	expect(request?.name).toBe("video");
	// ...so only one on-demand request is emitted for it.
	expect(await pendingRequest(consumer)).toBeUndefined();

	// Serving that single request fans out to both subscribers.
	if (!request) throw new Error("expected request");
	const producer = request.accept({ timescale: Timescale.MILLI });
	producer.writeString("hello");
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");

	// A different track still opens its own request.
	consumer.track("audio").subscribe();
	expect((await pendingRequest(consumer))?.name).toBe("audio");

	// Once the shared track closes, a later subscribe re-opens it.
	producer.close();
	consumer.track("video").subscribe();
	expect((await pendingRequest(consumer))?.name).toBe("video");
});

// Shared sequence namespace across producer replacements: the TypeScript equivalent of
// cloned Rust producers, so insertDatagram advances the next append for the replacement.
test("dynamic track sequences continue across producer replacements", async () => {
	const broadcast = new BroadcastProducer();

	// Pull before subscribing: a broadcast nobody serves on demand answers NotFound instead.
	const firstPull = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await firstPull;
	if (!firstRequest) throw new Error("expected first request");
	const firstProducer = firstRequest.accept({ timescale: Timescale.MILLI });
	expect(firstProducer.appendGroup().sequence).toBe(0);
	expect(firstProducer.appendDatagram(Timestamp.fromMillis(0), new Uint8Array())).toBe(1);
	firstProducer.writeGroup(new GroupProducer(8));
	firstProducer.insertDatagram(12, Timestamp.fromMillis(0), new Uint8Array());
	firstSubscriber.close();
	firstProducer.close();

	const secondSubscriber = broadcast.track("media").subscribe();
	const secondRequest = await wireOf(broadcast).requested();
	if (!secondRequest) throw new Error("expected second request");
	const secondProducer = secondRequest.accept({ timescale: Timescale.MILLI });
	expect(secondProducer.appendGroup().sequence).toBe(13);

	const nextGeneration = new BroadcastProducer();
	const nextPull = wireOf(nextGeneration).requested();
	const nextSubscriber = nextGeneration.track("media").subscribe();
	const nextRequest = await nextPull;
	if (!nextRequest) throw new Error("expected next-generation request");
	expect(nextRequest.accept({ timescale: Timescale.MILLI }).appendGroup().sequence).toBe(0);

	secondSubscriber.close();
	secondProducer.close();
	nextSubscriber.close();
	broadcast.close();
	nextGeneration.close();
});

test("concurrent dynamic producers share a sequence namespace", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const secondSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await pulled;
	const secondRequest = await wireOf(broadcast).requested();
	if (!firstRequest || !secondRequest) throw new Error("expected requests");
	const firstProducer = firstRequest.accept({ timescale: Timescale.MILLI });
	const secondProducer = secondRequest.accept({ timescale: Timescale.MILLI });

	expect(firstProducer.appendGroup().sequence).toBe(0);
	expect(secondProducer.appendGroup().sequence).toBe(1);
	expect(firstProducer.appendGroup().sequence).toBe(2);

	firstSubscriber.close();
	secondSubscriber.close();
	firstProducer.close();
	secondProducer.close();
	broadcast.close();
});

test("a sibling producer's groups do not settle an aborted end", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const secondSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await pulled;
	const secondRequest = await wireOf(broadcast).requested();
	if (!firstRequest || !secondRequest) throw new Error("expected requests");
	const firstProducer = firstRequest.accept({ timescale: Timescale.MILLI });
	const secondProducer = secondRequest.accept({ timescale: Timescale.MILLI });

	const reader = firstProducer.subscribe();
	firstProducer.finishAt(3);
	for (let i = 0; i < 3; i++) secondProducer.appendGroup().close();
	const boom = new Error("boom");
	firstProducer.close(boom);

	// The first producer never received the groups its end promised, so it was cut off.
	expect(firstProducer.closed.peek()).toBe(boom);
	await expect(reader.recvGroup()).rejects.toBe(boom);

	firstSubscriber.close();
	secondSubscriber.close();
	secondProducer.close();
	broadcast.close();
});

test("a clean close ends at the track's own last group, not a sibling's", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const secondSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await pulled;
	const secondRequest = await wireOf(broadcast).requested();
	if (!firstRequest || !secondRequest) throw new Error("expected requests");
	const firstProducer = firstRequest.accept({ timescale: Timescale.MILLI });
	const secondProducer = secondRequest.accept({ timescale: Timescale.MILLI });

	const reader = firstProducer.subscribe();
	firstProducer.appendGroup().close();
	for (let i = 0; i < 3; i++) secondProducer.appendGroup().close();
	firstProducer.close();

	// The sibling took 1..3, which the first producer will never send.
	expect(reader.final()).toBe(1);

	firstSubscriber.close();
	secondSubscriber.close();
	secondProducer.close();
	broadcast.close();
});

test("finishAt accepts an end below a sibling's groups", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const secondSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await pulled;
	const secondRequest = await wireOf(broadcast).requested();
	if (!firstRequest || !secondRequest) throw new Error("expected requests");
	const firstProducer = firstRequest.accept({ timescale: Timescale.MILLI });
	const secondProducer = secondRequest.accept({ timescale: Timescale.MILLI });

	const reader = firstProducer.subscribe();
	firstProducer.appendGroup().close();
	for (let i = 0; i < 3; i++) secondProducer.appendGroup().close();
	expect(() => firstProducer.finishAt(0)).toThrow("track end 0 is below the next sequence 1");
	firstProducer.finishAt(2);
	expect(reader.final()).toBe(2);

	firstSubscriber.close();
	secondSubscriber.close();
	firstProducer.close();
	secondProducer.close();
	broadcast.close();
});

test("closing a broadcast rejects a dequeued request", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const subscriber = broadcast.track("media").subscribe();
	const request = await pulled;
	if (!request) throw new Error("expected request");
	broadcast.close();
	await expect(subscriber.info()).rejects.toThrow("track closed before info was known");

	const producer = request.accept({ timescale: Timescale.MILLI });
	expect(() => producer.appendGroup()).toThrow("track is closed");
	await expect(subscriber.info()).rejects.toThrow("track closed before info was known");
	subscriber.close();
	producer.close();
});

test("a request exposes the aggregate subscription options", async () => {
	const consumer = new TestConsumer();

	consumer.track("video").subscribe({
		priority: 3,
		maxDelay: Milli(100),
		groups: { start: { included: 10 }, end: { excluded: 20 } },
	});
	consumer.track("video").subscribe({
		priority: 7,
		maxDelay: Milli(250),
		groups: { start: { included: 0 }, end: { excluded: 30 } },
	});

	const request = await pendingRequest(consumer);
	expect(request?.subscription).toEqual({
		priority: 7,
		maxDelay: Milli(250),
		groups: { start: { included: 0 }, end: { excluded: 30 } },
	});
	expect(request?.priority).toBe(7);
});

test("requested selects the highest current priority", async () => {
	const consumer = new TestConsumer();

	const first = consumer.track("first").subscribe({ priority: 1 });
	consumer.track("second").subscribe({ priority: 5 });
	const updated = first.subscription.changed();
	first.update({ priority: 9 });
	await updated;

	expect((await wireOf(consumer).requested())?.name).toBe("first");
	expect((await wireOf(consumer).requested())?.name).toBe("second");
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
	const a = consumer.track("video").subscribe().ordered();
	const b = consumer.track("video").subscribe().ordered();

	const request = await pendingRequest(consumer);
	if (!request) throw new Error("expected request");
	expect(await pendingRequest(consumer)).toBeUndefined();

	const producer = request.accept({ timescale: Timescale.MILLI });
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

	const track1 = new TrackProducer("track1").accept({ timescale: Timescale.MILLI });
	broadcast.insertTrack(track1);
	track1.appendGroup().close();

	// The track already exists, so subscribe resolves immediately (no requested()).
	const sub1 = broadcast.track("track1").subscribe().ordered();
	expect((await sub1.nextGroup())?.sequence).toBe(0);

	// No on-demand request was emitted for it.
	expect(await pendingRequest(broadcast)).toBeUndefined();

	// A second static track behaves the same.
	const track2 = new TrackProducer("track2").accept({ timescale: Timescale.MILLI });
	broadcast.insertTrack(track2);

	const sub2 = broadcast.track("track2").subscribe().ordered();
	track2.appendGroup().close();
	expect((await sub2.nextGroup())?.sequence).toBe(0);
});

test("two subscribers to one inserted track each get a full copy", async () => {
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const a = broadcast
		.track("video")
		.subscribe({ maxDelay: Milli(5000) })
		.ordered();
	const b = broadcast
		.track("video")
		.subscribe({ maxDelay: Milli(5000) })
		.ordered();

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
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	// Written before anyone subscribes; retained in the cache for replay.
	producer.writeString("early");

	const late = broadcast.track("video").subscribe().ordered();
	expect(await late.readString()).toBe("early");

	producer.writeString("later");
	expect(await late.readString()).toBe("later");
});

test("a read throws GroupTooLarge on an overflow, then resyncs to the next group", async () => {
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const sub = broadcast
		.track("video")
		.subscribe({ maxDelay: Milli(5000) })
		.ordered();

	// Group 0 overflows its frame cap: the group is aborted.
	const g0 = producer.appendGroup();
	for (let i = 0; i < MAX_GROUP_FRAMES; i++)
		g0.writeFrame({ payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	expect(() => g0.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.now() })).toThrow(GroupTooLarge);

	// Group 1 is clean.
	const g1 = producer.appendGroup();
	g1.writeFrame({ payload: new TextEncoder().encode("ok"), timestamp: Timestamp.now() });
	g1.close();

	// The reader hits the abort in group 0 (error, not a silent skip), then the next
	// read resyncs from group 1.
	expect(sub.readFrame()).rejects.toBeInstanceOf(GroupTooLarge);
	expect(await sub.readString()).toBe("ok");
});

test("a stalled consumer does not pin evicted groups", async () => {
	try {
		setSystemTime(new Date(10_000));

		const broadcast = new BroadcastProducer();
		const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: Milli(1000) });

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

	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: Milli(2000), priority: 3 });
	expect(producer.name).toBe("video");

	const info = await broadcast.track("video").info();
	expect(info.maxAge).toBe(Milli(2000));
	expect(info.priority).toBe(3);
});

test("insertTrack rejects a duplicate live name", () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("dup", { timescale: Timescale.MILLI });
	expect(() => broadcast.insertTrack(new TrackProducer("dup").accept({ timescale: Timescale.MILLI }))).toThrow();
});

test("a finished track is still served from its cache", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("track1", { timescale: Timescale.MILLI });
	track.writeString("last");
	track.close();

	// Like Rust, finishing keeps the track: a late subscriber reads the cache, then the clean end.
	const late = broadcast
		.consume()
		.track("track1")
		.subscribe({ maxDelay: Milli(5000) })
		.ordered();
	expect(await late.readString()).toBe("last");
	expect(await late.nextGroup()).toBeUndefined();
	expect(await broadcast.track("track1").info()).toBeDefined();
	expect(await pendingRequest(broadcast)).toBeUndefined();
});

test("an aborted track is gone, so a subscribe with no handler answers NotFound", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("track1").close(new Error("boom"));

	// Nothing serves requests on demand, so this must fail rather than wait forever.
	const subscriber = broadcast.consume().track("track1").subscribe();
	await expect(subscriber.info()).rejects.toBeInstanceOf(NotFound);
	await expect(subscriber.recvGroup()).rejects.toBeInstanceOf(NotFound);
	await expect(broadcast.track("track1").info()).rejects.toBeInstanceOf(NotFound);
});

test("an aborted track falls through to a request when a handler is serving", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	broadcast.createTrack("track1").close(new Error("boom"));

	broadcast.track("track1").subscribe();
	expect((await pulled)?.name).toBe("track1");
});

test("removeTrack drops the static entry", async () => {
	// While the static entry exists, subscribing takes the fast path: no on-demand request.
	const kept = new BroadcastProducer();
	kept.createTrack("track1", { timescale: Timescale.MILLI });
	kept.track("track1").subscribe();
	expect(await pendingRequest(kept)).toBeUndefined();

	// With the entry removed and nothing serving on demand, the track is not found.
	const removed = new BroadcastProducer();
	removed.createTrack("track1", { timescale: Timescale.MILLI });
	removed.removeTrack("track1");
	await expect(removed.track("track1").subscribe().info()).rejects.toBeInstanceOf(NotFound);
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

// A fetch parks for a group that has yet to be published, which is what the consuming wire
// layer's coalescing relies on. The publisher's fill deliberately does not use this path: it
// wants a group that already exists, and waiting for one that is gone would never return.
test("a fetch waits for a group still to come", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const pending = wireOf(broadcast).fetchGroup("video", 1);

	const first = track.appendGroup();
	first.writeFrame({ payload: new TextEncoder().encode("0"), timestamp: Timestamp.now() });
	first.close();

	const second = track.appendGroup();
	second.writeFrame({ payload: new TextEncoder().encode("1"), timestamp: Timestamp.now() });
	second.close();

	const group = await pending;
	expect(group.sequence).toBe(1);
	expect(await group.readString()).toBe("1");

	broadcast.close();
});

test("aborting a fetch rejects with the signal's reason", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const early = new Error("early");
	await expect(wireOf(broadcast).fetchGroup("video", 0, { signal: AbortSignal.abort(early) })).rejects.toBe(early);

	const controller = new AbortController();
	const pending = wireOf(broadcast).fetchGroup("video", 0, { signal: controller.signal });
	const late = new Error("late");
	controller.abort(late);
	await expect(pending).rejects.toBe(late);

	broadcast.close();
});

test("broadcast demand watches static and pending tracks rather than broadcast handles", async () => {
	const broadcast = new BroadcastProducer();
	const demand = broadcast.demand();
	const consumer = broadcast.consume();
	expect(demand.used.peek()).toBe(false);
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const first = video.subscribe();
	await Promise.resolve();
	expect(demand.used.peek()).toBe(true);
	const pulled = wireOf(broadcast).requested();
	const second = consumer.track("audio").subscribe();
	const request = await pulled;
	if (!request) throw new Error("expected request");
	const requested = request.demand();
	expect(requested.used.peek()).toBe(true);
	first.close();
	await Promise.resolve();
	expect(demand.used.peek()).toBe(true);
	second.close();
	await requested.unused();
	await demand.unused();
	expect(demand.used.peek()).toBe(false);
	request.reject();
	video.close();
	broadcast.close();
	expect(await demand.closed).toBeNull();
	consumer.close();
});

test("broadcast closure clears active demand and subscriptions", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const subscriber = track.subscribe();
	const demand = broadcast.demand();
	await Promise.resolve();
	expect(demand.used.peek()).toBe(true);
	broadcast.close();
	expect(demand.used.peek()).toBe(false);
	await demand.unused();
	subscriber.close();
	track.close();
});

test("a pending track info query counts as broadcast demand", async () => {
	const broadcast = new BroadcastProducer();
	const demand = broadcast.demand();

	const pending = wireOf(broadcast).requested();
	const info = wireOf(broadcast).resolveTrackInfo("video");
	expect(demand.used.peek()).toBe(true);

	const request = await pending;
	if (!request) throw new Error("expected request");
	// Nobody subscribes to the queried track itself.
	expect(request.demand().used.peek()).toBe(false);
	request.accept({ timescale: Timescale.MILLI, priority: 2 });
	expect((await info).priority).toBe(2);
	await demand.unused();
	expect(demand.used.peek()).toBe(false);

	// Closing the broadcast ends a query's demand at once.
	const rejected = wireOf(broadcast).resolveTrackInfo("audio");
	expect(demand.used.peek()).toBe(true);
	broadcast.close();
	expect(demand.used.peek()).toBe(false);
	await expect(rejected).rejects.toThrow();
});

test("removeTrack stops counting the removed track's demand at once", async () => {
	const broadcast = new BroadcastProducer();
	const demand = broadcast.demand();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const subscriber = video.subscribe();
	await Promise.resolve();
	expect(demand.used.peek()).toBe(true);

	broadcast.removeTrack("video");
	expect(demand.used.peek()).toBe(false);

	// Re-inserting the same track counts it again.
	broadcast.insertTrack(video);
	expect(demand.used.peek()).toBe(true);

	subscriber.close();
	video.close();
	broadcast.close();
});

test("inserting a closed track leaves no demand watcher behind", () => {
	const broadcast = new BroadcastProducer();
	const track = new TrackProducer("video").accept({ timescale: Timescale.MILLI });
	track.close();

	// Count the listeners insertTrack attaches and never disposes.
	let live = 0;
	const subscribe = Signal.prototype.subscribe;
	const spy = spyOn(Signal.prototype, "subscribe").mockImplementation(function (this: Signal<unknown>, fn) {
		live++;
		const dispose = subscribe.call(this, fn);
		return () => {
			live--;
			dispose();
		};
	});
	try {
		broadcast.insertTrack(track);
	} finally {
		spy.mockRestore();
	}

	expect(live).toBe(0);
	expect(broadcast.demand().used.peek()).toBe(false);
	broadcast.close();
});
