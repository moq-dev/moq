import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import * as cache from "./cache.ts";
import { defined } from "./defined.test-util.ts";
import { NotFound, StreamCode, TooFarBehind } from "./error.ts";
import { hooks } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { everyRange, withMax } from "./subscription.test-util.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Request as TrackRequest } from "./track.ts";
import { unwanted } from "./util/demand.ts";
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
	producer.writeFrame(textFrame("hello"));
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");

	// A different track still opens its own request.
	consumer.track("audio").subscribe();
	expect((await pendingRequest(consumer))?.name).toBe("audio");

	// Once the shared track closes, a later subscribe re-opens it.
	producer.finish();
	producer.close();
	consumer.track("video").subscribe();
	expect((await pendingRequest(consumer))?.name).toBe("video");
});

test("a staged track resolves its subscribers at commit, with what was written", async () => {
	const consumer = new TestConsumer();
	const track = consumer.track("video");
	const reader = track.subscribe().ordered();
	const request = await pendingRequest(consumer);
	if (!request) throw new Error("expected request");

	const staged = hooks.stageRequest(request, { timescale: Timescale.MILLI });
	// The staged track has no timescale until commit, so the group is created at its key.
	const group = staged.producer.createGroup(0n);
	group.writeFrame(textFrame("hello", Timestamp.fromMillis(0)));
	group.finish();
	const pending = Symbol("pending");
	expect(await Promise.race([track.info(), Promise.resolve(pending)])).toBe(pending);

	staged.commit();
	expect((await track.info()).timescale).toBe(Timescale.MILLI);
	expect(await reader.readString()).toBe("hello");
});

// A broadcast keeps nothing of a name once its track is gone: a replacement numbers and times its
// groups from scratch, and the application owns keeping them past its predecessor's.
test("a replacement producer starts over", async () => {
	const broadcast = new BroadcastProducer();

	// Pull before subscribing: a broadcast nobody serves on demand answers NotFound instead.
	const firstPull = wireOf(broadcast).requested();
	const firstSubscriber = broadcast.track("media").subscribe();
	const firstRequest = await firstPull;
	if (!firstRequest) throw new Error("expected first request");
	const first = firstRequest.accept({ timescale: Timescale.MILLI });
	const group = first.createGroup(8n);
	group.writeFrame(textFrame("a", Timestamp.fromMillis(1000)));
	group.finish();
	firstSubscriber.close();
	first.finish();
	first.close();

	const secondSubscriber = broadcast.track("media").subscribe();
	const secondRequest = await wireOf(broadcast).requested();
	if (!secondRequest) throw new Error("expected second request");
	const second = secondRequest.accept({ timescale: Timescale.MILLI });
	const appended = hooks.appendNext(second);
	expect(appended.start).toBe(0n);
	appended.writeFrame(textFrame("b", Timestamp.fromMillis(500)));
	secondSubscriber.close();
	second.finish();
	second.close();

	broadcast.removeTrack("media");
	const third = defined(broadcast.createTrack("media", { timescale: Timescale.MILLI }));
	third.finishAt({ group: 0n, frame: 0 });
	third.close();
	broadcast.close();
});

test("createTrack answers a queued request", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const info = broadcast.track("media").info();
	const subscriber = broadcast.track("media").subscribe().ordered();

	// The track is created before any handler takes the queued request.
	const producer = defined(broadcast.createTrack("media", { timescale: Timescale.MILLI, priority: 3 }));
	producer.writeFrame(textFrame("hello", Timestamp.fromMillis(1)));
	expect(await subscriber.readString()).toBe("hello");
	expect((await info).priority).toBe(3);

	// No handler ever sees the request, and the lookup that opened it leaves the track open.
	expect(await pendingRequest(broadcast)).toBeUndefined();
	subscriber.close();
	await unwanted(producer.demand());
	expect(producer.closed.peek()).toBeUndefined();

	// Later subscriptions read the created track, and the name stays taken.
	const again = broadcast.track("media").subscribe().ordered();
	producer.writeFrame(textFrame("again", Timestamp.fromMillis(2)));
	expect(await again.readString()).toBe("again");
	expect(broadcast.createTrack("media")).toBeUndefined();

	broadcast.close();
	expect(await pulled).toBeUndefined();
});

test("createTrack with invalid info rejects the queued request it answers", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const subscriber = broadcast.track("media").subscribe();

	expect(() => broadcast.createTrack("media", { priority: 256 })).toThrow();
	expect(await subscriber.closed).toBeInstanceOf(Error);

	// The refused request no longer holds the name.
	defined(broadcast.createTrack("media"));
	broadcast.close();
	expect(await pulled).toBeUndefined();
});

test("createTrack does not take over a request a handler already pulled", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	broadcast.track("media").subscribe();
	const request = await pulled;
	if (!request) throw new Error("expected request");

	// The handler holds it, so the name stays duplicate until it answers, as in Rust.
	expect(broadcast.createTrack("media")).toBeUndefined();
	request.reject(new Error("request refused"));
	broadcast.close();
});

test("publishing-side subscriptions coalesce onto one request", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const info = broadcast.track("media").info();
	const a = broadcast.track("media").subscribe().ordered();
	const b = broadcast.track("media").subscribe().ordered();

	// One request serves the info lookup and every subscriber.
	const request = await pulled;
	expect(request?.name).toBe("media");
	expect(await pendingRequest(broadcast)).toBeUndefined();

	if (!request) throw new Error("expected request");
	const producer = request.accept({ timescale: Timescale.MILLI });
	producer.writeFrame(textFrame("hello"));
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");
	await info;
	// The lookup leaves the shared producer to its subscribers.
	expect(producer.closed.peek()).toBeUndefined();
	a.close();
	b.close();
	await unwanted(producer.demand());
	expect(producer.closed.peek()).toBeUndefined();

	producer.finish();
	producer.close();
	broadcast.close();
});

test("an info lookup joining a subscription's request is answered after the subscriber leaves", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const subscriber = broadcast.track("media").subscribe();
	const info = broadcast.track("media").info();
	const request = await pulled;
	if (!request) throw new Error("expected request");

	// Flush every pending notification, so only the lookup is left holding demand.
	subscriber.close();
	await yieldNow();
	expect(request.demand().wanted.peek()).toBe(true);
	expect(hooks.demandSubscribed(request.demand())).toBe(false);

	request.accept({ timescale: Timescale.MILLI, priority: 2 });
	expect((await info).priority).toBe(2);
	broadcast.close();
});

test("an info lookup releases a request nobody subscribed to", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const info = broadcast.track("media").info();
	const request = await pulled;
	if (!request) throw new Error("expected request");
	const producer = request.accept();
	await info;
	expect(producer.closed.peek()).toBeDefined();

	// The next subscription asks again.
	const subscriber = broadcast.track("media").subscribe();
	expect((await pendingRequest(broadcast))?.name).toBe("media");
	subscriber.close();
	broadcast.close();
});

test("held info lookups keep a request until the last one lets go", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const first = new AbortController();
	const opened = wireOf(broadcast).resolveTrackInfo("media", first.signal);
	const request = await pulled;
	if (!request) throw new Error("expected request");
	expect(request.demand().wanted.peek()).toBe(true);
	const producer = request.accept();
	const second = new AbortController();
	await wireOf(broadcast).resolveTrackInfo("media", second.signal);
	await opened;

	// The lookup that opened the request lets go first, under the one still holding it.
	first.abort();
	expect(producer.closed.peek()).toBeUndefined();
	// An answered lookup still held is the accepted track's demand, as a subscriber is.
	expect(producer.demand().wanted.peek()).toBe(true);

	second.abort();
	expect(producer.demand().wanted.peek()).toBe(false);
	expect(producer.closed.peek()).toBeDefined();
	broadcast.close();
});

test("a held info lookup stops counting as demand once its track closes", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const first = new AbortController();
	const opened = wireOf(broadcast).resolveTrackInfo("media", first.signal);
	const request = await pulled;
	if (!request) throw new Error("expected request");
	const producer = request.accept({ timescale: Timescale.MILLI });
	const second = new AbortController();
	await wireOf(broadcast).resolveTrackInfo("media", second.signal);
	await opened;
	expect(producer.demand().wanted.peek()).toBe(true);

	producer.close();
	expect(producer.demand().wanted.peek()).toBe(false);
	// A hold letting go after the close reports demand again, which stays off.
	first.abort();
	expect(producer.demand().wanted.peek()).toBe(false);
	second.abort();
	broadcast.close();
});

test("an abandoned info lookup's request goes once answered", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const hold = new AbortController();
	const info = wireOf(broadcast).resolveTrackInfo("media", hold.signal);
	const request = await pulled;
	if (!request) throw new Error("expected request");

	hold.abort();
	await expect(info).rejects.toThrow();

	// The handler still gets an open track, and the answer lets it go.
	const producer = request.accept({ timescale: Timescale.MILLI });
	producer.writeFrame(textFrame("late"));
	await producer.closed;
	broadcast.close();
});

test("a held info lookup on an inserted track leaves it open when it lets go", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("media", { timescale: Timescale.MILLI }));
	const hold = new AbortController();
	await wireOf(broadcast).resolveTrackInfo("media", hold.signal);

	hold.abort();
	expect(track.closed.peek()).toBeUndefined();
	broadcast.close();
});

test("removeTrack leaves a track that only a request serves", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const subscriber = broadcast.track("media").subscribe();
	const request = await pulled;
	if (!request) throw new Error("expected request");
	const producer = request.accept();

	broadcast.removeTrack("media");
	expect(producer.closed.peek()).toBeUndefined();

	subscriber.close();
	producer.finish();
	producer.close();
	broadcast.close();
});

test("closing a broadcast rejects a dequeued request", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	const subscriber = broadcast.track("media").subscribe();
	const request = await pulled;
	if (!request) throw new Error("expected request");
	broadcast.close();
	await expect(subscriber.info()).rejects.toMatchObject({ code: StreamCode.Internal });

	const producer = request.accept({ timescale: Timescale.MILLI });
	expect(() => hooks.appendNext(producer)).toThrow("track is closed");
	await expect(subscriber.info()).rejects.toMatchObject({ code: StreamCode.Internal });
	subscriber.close();
	producer.finish();
	producer.close();
});

test("a request exposes the aggregate subscription options", async () => {
	const consumer = new TestConsumer();

	consumer.track("video").subscribe(
		withMax({
			priority: 3,
			delay: { max: Duration.millis(100) },
			ranges: [{ start: 10n, end: 19n }],
		}),
	);
	consumer.track("video").subscribe(
		withMax({
			priority: 7,
			delay: { max: Duration.millis(250) },
			ranges: [{ start: 0n, end: 29n }],
		}),
	);

	const request = await pendingRequest(consumer);
	expect(request?.subscription).toEqual(
		withMax({
			priority: 7,
			delay: { max: Duration.millis(250) },
			order: "descending",
			ranges: [{ start: 0n, end: 29n }],
		}),
	);
	expect(request?.priority).toBe(7);
});

test("requested selects the highest current priority", async () => {
	const consumer = new TestConsumer();

	const first = consumer.track("first").subscribe(everyRange({}, { priority: 1 }));
	consumer.track("second").subscribe(everyRange({}, { priority: 5 }));
	const updated = first.subscription.changed();
	first.update(everyRange({}, { priority: 9 }));
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

test("closing every consumer leaves a produced broadcast to its producer", async () => {
	const producer = new BroadcastProducer();
	const track = defined(producer.createTrack("video"));
	const consumer = producer.consume();
	const subscriber = consumer.track("video").subscribe();
	consumer.close();
	subscriber.close();
	expect(producer.closed.peek()).toBeUndefined();

	track.close();
	producer.close();
	expect(producer.closed.peek()).toBeNull();
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
	producer.writeFrame(textFrame("one", Timestamp.fromMillis(1)));
	expect(await a.readString()).toBe("one");
	expect(await b.readString()).toBe("one");

	// ...and closing one subscriber leaves the other (and the shared upstream) delivering.
	a.close();
	producer.writeFrame(textFrame("two", Timestamp.fromMillis(2)));
	expect(await b.readString()).toBe("two");
});

test("subscribe serves a statically inserted track without a request", async () => {
	const broadcast = new BroadcastProducer();

	const track1 = defined(broadcast.createTrack("track1", { timescale: Timescale.MILLI }));
	hooks.appendNext(track1).finish();

	// The track already exists, so subscribe resolves immediately (no requested()).
	const sub1 = broadcast.track("track1").subscribe().ordered();
	expect((await sub1.nextGroup())?.start).toBe(0n);

	// No on-demand request was emitted for it.
	expect(await pendingRequest(broadcast)).toBeUndefined();

	// A second static track behaves the same.
	const track2 = defined(broadcast.createTrack("track2", { timescale: Timescale.MILLI }));

	const sub2 = broadcast.track("track2").subscribe().ordered();
	hooks.appendNext(track2).finish();
	expect((await sub2.nextGroup())?.start).toBe(0n);
});

test("two subscribers to one inserted track each get a full copy", async () => {
	const broadcast = new BroadcastProducer();
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));

	const a = broadcast
		.track("video")
		.subscribe(withMax({ delay: { max: Duration.millis(5000) } }))
		.ordered();
	const b = broadcast
		.track("video")
		.subscribe(withMax({ delay: { max: Duration.millis(5000) } }))
		.ordered();

	producer.writeFrame(textFrame("hello", Timestamp.fromMillis(1)));
	producer.writeFrame(textFrame("world", Timestamp.fromMillis(2)));

	// Neither subscriber steals from the other: both see every frame in order.
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");
	expect(await a.readString()).toBe("world");
	expect(await b.readString()).toBe("world");
});

test("a late subscriber replays the cached window", async () => {
	const broadcast = new BroadcastProducer();
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));

	// Written before anyone subscribes; retained in the cache for replay.
	producer.writeFrame(textFrame("early", Timestamp.fromMillis(1)));

	const late = broadcast.track("video").subscribe().ordered();
	expect(await late.readString()).toBe("early");

	producer.writeFrame(textFrame("later", Timestamp.fromMillis(2)));
	expect(await late.readString()).toBe("later");
});

test("a read skips a group whose front was shed, as it does any failed group", async () => {
	const broadcast = new BroadcastProducer();
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const sub = broadcast
		.track("video")
		.subscribe(withMax({ delay: { max: Duration.millis(5000) } }))
		.ordered();

	// Group 0 sheds its front without aborting the writer.
	const g0 = hooks.appendNext(producer);
	for (let i = 0; i < 40; i++)
		g0.writeFrame({ payload: new Uint8Array(1024 * 1024), timestamp: Timestamp.fromMillis(0) });

	// Group 1 is clean.
	const g1 = hooks.appendNext(producer);
	g1.writeFrame({ payload: new TextEncoder().encode("ok"), timestamp: Timestamp.fromMillis(1) });
	g1.finish();

	// The reader hits the shed front in group 0, a gap it reads past to group 1.
	expect(await sub.readString()).toBe("ok");
});

test("a stalled consumer does not pin evicted groups", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		clock.jump(Duration.millis(10_000));

		const broadcast = new BroadcastProducer();
		const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));

		// A subscriber that never reads. Its sink must not grow without bound. No max delay,
		// so only eviction (not a skip of a superseded group) can drop the old group.
		const stalled = broadcast.track("video").subscribe(withMax({ delay: { max: null } }));

		producer.writeFrame(textFrame("old"));

		// Advance past the cache window and write again, which scans for idle groups. The old
		// (closed, aged-out) group is dropped from the stalled sink, not retained.
		clock.jump(Duration.millis(30_000 + 1_000));
		producer.writeFrame(textFrame("fresh"));

		// The next group in arrival order is the fresh one (starting at 41s): the old (10s) group
		// was evicted, not still buffered ahead of it.
		expect((await stalled.recvGroup())?.start).toBe(41_000n);

		// Nothing else is buffered: a second recvGroup stays pending (the track is open),
		// proving exactly one group remained in the sink rather than two.
		const pending = Symbol("pending");
		const next = await Promise.race([stalled.recvGroup(), Promise.resolve(pending)]);
		expect(next).toBe(pending);
	} finally {
		installed.close();
	}
});

test("createTrack commits info up front", async () => {
	const broadcast = new BroadcastProducer();

	const producer = defined(
		broadcast.createTrack("video", {
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(2000),
			priority: 3,
		}),
	);
	expect(producer.name).toBe("video");

	const info = await broadcast.track("video").info();
	expect(info.maxAge).toEqual(Duration.millis(2000));
	expect(info.priority).toBe(3);
});

test("createTrack returns undefined for a duplicate live name", () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("dup", { timescale: Timescale.MILLI }));
	expect(broadcast.createTrack("dup", { timescale: Timescale.MILLI })).toBeUndefined();
});

test("a finished track is still served from its cache", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("track1", { timescale: Timescale.MILLI }));
	track.writeFrame(textFrame("last"));
	track.finish();
	track.close();

	// Like Rust, finishing keeps the track: a late subscriber reads the cache, then the clean end.
	const late = broadcast
		.consume()
		.track("track1")
		.subscribe(withMax({ delay: { max: Duration.millis(5000) } }))
		.ordered();
	expect(await late.readString()).toBe("last");
	expect(await late.nextGroup()).toBeUndefined();
	expect(await broadcast.track("track1").info()).toBeDefined();
	expect(await pendingRequest(broadcast)).toBeUndefined();
});

test("an aborted track is gone, so a subscribe with no handler answers NotFound", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("track1")).abort(new Error("boom"));

	// Nothing serves requests on demand, so this must fail rather than wait forever.
	const subscriber = broadcast.consume().track("track1").subscribe();
	await expect(subscriber.info()).rejects.toBeInstanceOf(NotFound);
	await expect(subscriber.recvGroup()).rejects.toBeInstanceOf(NotFound);
	await expect(broadcast.track("track1").info()).rejects.toBeInstanceOf(NotFound);
});

test("an aborted track falls through to a request when a handler is serving", async () => {
	const broadcast = new BroadcastProducer();
	const pulled = wireOf(broadcast).requested();
	defined(broadcast.createTrack("track1")).abort(new Error("boom"));

	broadcast.track("track1").subscribe();
	expect((await pulled)?.name).toBe("track1");
});

test("removeTrack drops the static entry", async () => {
	// While the static entry exists, subscribing takes the fast path: no on-demand request.
	const kept = new BroadcastProducer();
	defined(kept.createTrack("track1", { timescale: Timescale.MILLI }));
	kept.track("track1").subscribe();
	expect(await pendingRequest(kept)).toBeUndefined();

	// With the entry removed and nothing serving on demand, the track is not found.
	const removed = new BroadcastProducer();
	defined(removed.createTrack("track1", { timescale: Timescale.MILLI }));
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
test("a fetch of a group still to come asks the request handler, and the write answers it", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	// As in Rust, the miss goes to the handler at once; without one it would be NotFound.
	const requests = track.requests();

	const pending = wireOf(broadcast).fetchGroup("video", 1n);

	const first = hooks.appendNext(track);
	first.writeFrame({ payload: new TextEncoder().encode("0"), timestamp: Timestamp.fromMillis(0) });
	first.finish();

	const second = hooks.appendNext(track);
	second.writeFrame({ payload: new TextEncoder().encode("1"), timestamp: Timestamp.fromMillis(1) });
	second.finish();

	const group = await pending;
	expect(group.start).toBe(1n);
	expect(await group.readString()).toBe("1");

	requests.close();
	broadcast.close();
});

test("aborting a fetch rejects with the signal's reason", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	// A request handler keeps the miss pending until the abort.
	const requests = track.requests();

	const early = new Error("early");
	await expect(wireOf(broadcast).fetchGroup("video", 0n, { signal: AbortSignal.abort(early) })).rejects.toBe(early);

	const controller = new AbortController();
	const pending = wireOf(broadcast).fetchGroup("video", 0n, { signal: controller.signal });
	const late = new Error("late");
	controller.abort(late);
	await expect(pending).rejects.toBe(late);

	requests.close();
	broadcast.close();
});

test("a pending track info query is answered by its request, and closing rejects it", async () => {
	const broadcast = new BroadcastProducer();

	const pending = wireOf(broadcast).requested();
	const info = wireOf(broadcast).resolveTrackInfo("video");

	const request = await pending;
	if (!request) throw new Error("expected request");
	// Nobody subscribes to the queried track itself, but the pending query is its demand.
	expect(request.demand().wanted.peek()).toBe(true);
	expect(hooks.demandSubscribed(request.demand())).toBe(false);
	request.accept({ timescale: Timescale.MILLI, priority: 2 });
	expect((await info).priority).toBe(2);

	const rejected = wireOf(broadcast).resolveTrackInfo("audio");
	broadcast.close();
	await expect(rejected).rejects.toThrow();
});

// A failed group's consumer settles `closed` with frames still unread; the fetch keeps its mirror
// registered until the caller lets go, so the pool can still reclaim those frames.
test("a fetch of a failed group leaves its unread frames to the pool's expiry", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
		hooks.poolTrack(track, new cache.Pool({ expiry: Duration.millis(1_000) }));

		const failed = hooks.appendNext(track);
		failed.writeFrame(textFrame("unread"));
		const group = await wireOf(broadcast).fetchGroup("video", 0n);
		// The publisher cuts the live group before the caller reads it.
		failed.abort(new Error("cut"));
		await yieldNow();
		expect(group.closed.peek()).toBeInstanceOf(Error);

		// The idle window reclaims the failed group, which the caller has not read.
		await clock.advance(Duration.millis(1_500));
		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("a track name with a lone surrogate is refused where it is named, since UTF-8 would write U+FFFD", () => {
	const broadcast = new BroadcastProducer();
	const consumer = broadcast.consume();
	for (const name of ["\uD800", "a\uDC00", "\uDC00\uD800"]) {
		expect(() => broadcast.createTrack(name)).toThrow("lone surrogate");
		expect(() => broadcast.createDatagrams(name)).toThrow("lone surrogate");
		expect(() => broadcast.track(name)).toThrow("lone surrogate");
		expect(() => consumer.track(name)).toThrow("lone surrogate");
		expect(() => consumer.datagrams(name)).toThrow("lone surrogate");
	}
	// A pair is one scalar, U+10000, and U+FFFD itself is text like any other.
	for (const name of ["𐀀", "a�"]) {
		expect(defined(broadcast.createTrack(name)).name).toBe(name);
		expect(consumer.track(name).name).toBe(name);
	}
	broadcast.close();
});
