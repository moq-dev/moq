import { expect, test } from "bun:test";
import type { Getter } from "@moq/signals";
import * as Announce from "./announced.ts";
import type { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect, Reload } from "./connection/index.ts";
import { StreamCode, StreamError } from "./error.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import type { Consumer as OriginConsumer } from "./origin.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Producer as TrackProducer } from "./track.ts";
import { withTimeout } from "./util/timeout.ts";

const url = new URL("https://localhost:4443/test");

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * The table's route for `path` as a handle of the caller's own.
 *
 * A request is the only way to consume by path, and it resolves against the table
 * synchronously, so a routed path needs no waiting. Cloned because a request only borrows.
 */
function routed(origin: OriginConsumer, path: Path.Valid): BroadcastConsumer | undefined {
	const request = origin.request(path);
	const front = request.active.peek()?.clone();
	request.close();
	return front;
}

async function runPublishSubscribeFlow(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version, publish: origin.consume() }),
	]);

	// Server publishes a broadcast
	const broadcast = origin.publish(Path.from("test"));
	const prefixedBroadcast = origin.publish(Path.from("root/child"));

	// Serve every requested "video" track. On lite-05+ a subscribe is preceded by
	// a TRACK info lookup, which the publisher answers by requesting the track too,
	// so more than one request can arrive; the publisher must accept() each.
	let served = 0;
	const serving = (async () => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			if (req.name !== "video") {
				req.reject(new Error(`unexpected track: ${req.name}`));
				continue;
			}
			served++;
			req.accept().writeString("hello");
		}
	})();

	// Client discovers announced broadcast
	const announced = client.announced();
	const entry = await announced.next();
	if (!entry) throw new Error("expected entry");
	expect(entry.path).toBe("test" as Path.Valid);
	expect(entry.active).toBe(true);

	// Prefix-scoped discovery returns paths relative to the requested prefix.
	const prefixed = client.announced(Path.from("root"));
	const prefixedEntry = await prefixed.next();
	if (!prefixedEntry) throw new Error("expected prefixed entry");
	expect(prefixedEntry.path).toBe("child" as Path.Valid);
	expect(prefixedEntry.active).toBe(true);

	// Client consumes the broadcast and subscribes to a track
	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Client reads data
	const data = await track.readString();
	expect(data).toBe("hello");
	expect(served).toBeGreaterThan(0);

	// Cleanup
	broadcast.close();
	prefixedBroadcast.close();
	await serving;
	announced.close();
	prefixed.close();
	remote.close();
	client.close();
	server.close();
}

test("integration: lite draft-01", async () => {
	await runPublishSubscribeFlow("", Lite.Version.DRAFT_01);
});

test("integration: lite draft-02", async () => {
	await runPublishSubscribeFlow("", Lite.Version.DRAFT_02);
});

test("integration: lite draft-03", async () => {
	await runPublishSubscribeFlow(Lite.ALPN_03);
});

test("integration: lite draft-05", async () => {
	// Exercises AnnounceOk: the announce flow only completes if the subscriber
	// reads the publisher's AnnounceOk before the initial Announce messages.
	await runPublishSubscribeFlow(Lite.ALPN_05);
});

test("integration: lite subscription options and updates reach the publisher", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));

	let resolveProducer: ((producer: TrackProducer) => void) | undefined;
	const accepted = new Promise<TrackProducer>((resolve) => {
		resolveProducer = resolve;
	});
	const serving = (async () => {
		for (;;) {
			const request = await broadcast.requested();
			if (!request) return;
			const producer = request.accept();
			if (request.subscription.startGroup === 0) resolveProducer?.(producer);
		}
	})();

	const remote = client.consume(Path.from("test"));
	const subscriber = remote.track("video").subscribe({
		priority: 3,
		ordered: true,
		maxAge: 250,
		startGroup: 0,
		endGroup: 9,
	});
	const producer = await accepted;
	expect(producer.subscription.peek()).toEqual({
		priority: 3,
		ordered: true,
		maxAge: 250,
		startGroup: 0,
		endGroup: 9,
	});

	const updated = producer.subscription.changed();
	subscriber.update({ priority: 8, ordered: false, maxAge: 500, startGroup: 2, endGroup: 12 });
	expect(await updated).toEqual({
		priority: 8,
		ordered: false,
		maxAge: 500,
		startGroup: 2,
		endGroup: 12,
	});

	subscriber.close();
	remote.close();
	broadcast.close();
	await serving;
	client.close();
	server.close();
});

test("integration: lite applies initial and updated group bounds", async () => {
	const GROUP_COUNT = 6;
	const INITIAL_START_GROUP = 1;
	const INITIAL_END_GROUP = 2;
	const UPDATED_GROUP = 4;
	const PENDING_ASSERT_MS = 20;
	const UPDATE_TIMEOUT_MS = 1000;

	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video");
	for (let sequence = 0; sequence < GROUP_COUNT; sequence++) producer.appendGroup().close();

	const remote = client.consume(Path.from("test"));
	const subscriber = remote
		.track("video")
		.subscribe({ startGroup: INITIAL_START_GROUP, endGroup: INITIAL_END_GROUP });
	try {
		expect((await subscriber.nextGroup())?.sequence).toBe(INITIAL_START_GROUP);
		expect((await subscriber.nextGroup())?.sequence).toBe(INITIAL_END_GROUP);

		const pending = subscriber.nextGroup();
		expect(await Promise.race([pending, sleep(PENDING_ASSERT_MS).then(() => "pending")])).toBe("pending");

		subscriber.update({ startGroup: UPDATED_GROUP, endGroup: UPDATED_GROUP });
		expect((await withTimeout(pending, UPDATE_TIMEOUT_MS, "updated group bound timed out"))?.sequence).toBe(
			UPDATED_GROUP,
		);

		const capped = subscriber.nextGroup();
		expect(await Promise.race([capped, sleep(PENDING_ASSERT_MS).then(() => "pending")])).toBe("pending");
	} finally {
		subscriber.close();
		remote.close();
		broadcast.close();
		client.close();
		server.close();
	}
});

test("integration: lite draft-06", async () => {
	// Exercises announce ids: every active assigns an ordinal on the wire.
	await runPublishSubscribeFlow(Lite.ALPN_06_WIP);
});

test("integration: lite draft-06 announce lifecycle", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	// Announced before the client asks, so it can ride the initial set.
	const first = origin.publish(Path.from("first"));

	const announced = client.announced();
	let entry = await announced.next();
	if (!entry) throw new Error("expected announce");
	expect(entry.path).toBe("first" as Path.Valid);
	expect(entry.active).toBe(true);

	// A live announce.
	const second = origin.publish(Path.from("second"));
	entry = await announced.next();
	if (!entry) throw new Error("expected announce");
	expect(entry.path).toBe("second" as Path.Valid);
	expect(entry.active).toBe(true);

	// Unannounce: retracted by announce id on the wire.
	second.close();
	entry = await announced.next();
	if (!entry) throw new Error("expected unannounce");
	expect(entry.path).toBe("second" as Path.Valid);
	expect(entry.active).toBe(false);

	// Re-announce the same path: a fresh announce assigning a fresh id.
	const secondAgain = origin.publish(Path.from("second"));
	entry = await announced.next();
	if (!entry) throw new Error("expected re-announce");
	expect(entry.path).toBe("second" as Path.Valid);
	expect(entry.active).toBe(true);

	// Cleanup
	first.close();
	secondAgain.close();
	announced.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 datagram delivery", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	// A static track fans datagrams out to whoever subscribes.
	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Datagrams aren't cached, so the first few may race the subscription setup. Pump until
	// the subscriber receives one, then stop.
	const received = track.recvDatagram();
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			producer.appendDatagram(Timestamp.fromMillis(i), enc.encode("dgram"));
			await sleep(2);
		}
	})();

	const got = await received;
	stop = true;
	await pump;

	expect(got).toBeDefined();
	expect(dec.decode(got?.payload)).toBe("dgram");

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 datagrams not sent on a non-datagram transport", async () => {
	const enc = new TextEncoder();
	// maxDatagramSize 0 simulates a qmux/WebSocket session: the publisher must fall back to
	// not sending datagrams (there is no group fallback), while groups still flow.
	const pair = createMockTransportPair(Lite.ALPN_05, { datagrams: false });
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Keep pushing a group (to prove the connection is live) and a datagram (which must be dropped).
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			producer.appendDatagram(Timestamp.fromMillis(i), enc.encode("dgram"));
			producer.writeString("group");
			await sleep(2);
		}
	})();

	// A group arrives, confirming the subscription works over this transport.
	const grp = await track.readString();
	expect(grp).toBe("group");

	// No datagram is ever delivered: recvDatagram stays pending until the timeout wins.
	const datagram = track.recvDatagram();
	datagram.catch(() => {}); // The track close below settles it; swallow to avoid a stray rejection.
	const outcome = await Promise.race([datagram, sleep(50).then(() => "timeout" as const)]);
	expect(outcome).toBe("timeout");

	stop = true;
	await pump;

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 datagrams sent with standards-track createWritable", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05, { datagramWritable: "createWritable" });
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	const received = track.recvDatagram();
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			producer.appendDatagram(Timestamp.fromMillis(i), enc.encode("dgram"));
			await sleep(2);
		}
	})();

	const got = await received;
	stop = true;
	await pump;

	expect(got).toBeDefined();
	expect(dec.decode(got?.payload)).toBe("dgram");

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 missing datagram writer does not close streams", async () => {
	const enc = new TextEncoder();
	const pair = createMockTransportPair(Lite.ALPN_05, { datagramWritable: "none" });
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	producer.appendDatagram(Timestamp.fromMillis(0), enc.encode("dgram"));
	producer.writeString("group");

	expect(await track.readString()).toBe("group");

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 missing datagram reader does not close streams", async () => {
	const enc = new TextEncoder();
	const pair = createMockTransportPair(Lite.ALPN_05, { datagramReadable: false });
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	producer.appendDatagram(Timestamp.fromMillis(0), enc.encode("dgram"));
	producer.writeString("group");

	expect(await track.readString()).toBe("group");

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

/** A stream reset as a transport delivers one: the peer's code, and nothing else useful. */
class Reset extends Error {
	readonly source = "stream" as const;
	readonly streamErrorCode: number;

	constructor(code: number) {
		super("");
		this.streamErrorCode = code;
	}
}

test("integration: a group reset carries the peer's code to the subscriber", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	const group = producer.appendGroup();
	group.writeString("frame");

	const consumer = await track.nextGroup();
	if (!consumer) throw new Error("expected a group");
	expect(await consumer.readString()).toBe("frame");

	// Reset the group stream the way a peer that dropped the group does.
	group.close(new Reset(2));

	// The subscriber reads the code back off a typed error rather than whichever shape the
	// transport produced, so nothing has to feature-detect a browser global.
	const err = await consumer.readFrame().then(
		() => undefined,
		(e: unknown) => e,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.DeliveryTimeout);

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 fetches a cached group", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video");

	const group0 = producer.appendGroup();
	group0.writeFrame({ payload: enc.encode("alpha"), timestamp: Timestamp.fromMillis(10) });
	group0.writeFrame({ payload: enc.encode("beta"), timestamp: Timestamp.fromMillis(15) });
	group0.close();

	const group1 = producer.appendGroup();
	group1.writeFrame({ payload: enc.encode("newer"), timestamp: Timestamp.fromMillis(20) });
	group1.close();

	// Fetch group 0 without holding a live subscription; the timestamps round-trip.
	const remote = client.consume(Path.from("test"));
	const fetched = await remote.track("video").fetchGroup(0);

	const first = await fetched.readFrame();
	expect(dec.decode(first?.payload)).toBe("alpha");
	expect(first?.timestamp.asMillis()).toBe(10);

	const second = await fetched.readFrame();
	expect(dec.decode(second?.payload)).toBe("beta");
	expect(second?.timestamp.asMillis()).toBe(15);

	expect(await fetched.readFrame()).toBeUndefined();

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 coalesces concurrent fetches of one group", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video");

	const group0 = producer.appendGroup();
	group0.writeFrame({ payload: enc.encode("alpha"), timestamp: Timestamp.fromMillis(10) });
	group0.writeFrame({ payload: enc.encode("beta"), timestamp: Timestamp.fromMillis(15) });
	group0.close();

	const remote = client.consume(Path.from("test"));
	const trackConsumer = remote.track("video");

	// Two concurrent fetches of the same group coalesce onto one FETCH stream; each reads an
	// independent mirror that still sees the full group.
	const [a, b] = await Promise.all([trackConsumer.fetchGroup(0), trackConsumer.fetchGroup(0)]);

	for (const fetched of [a, b]) {
		expect(dec.decode((await fetched.readFrame())?.payload)).toBe("alpha");
		expect(dec.decode((await fetched.readFrame())?.payload)).toBe("beta");
		expect(await fetched.readFrame()).toBeUndefined();
	}

	// After the coalesced fetch completes and its cache entry evicts, the same group re-fetches.
	const again = await trackConsumer.fetchGroup(0);
	expect(dec.decode((await again.readFrame())?.payload)).toBe("alpha");
	expect(dec.decode((await again.readFrame())?.payload)).toBe("beta");
	expect(await again.readFrame()).toBeUndefined();

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 fetches an in-progress group", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const producer = broadcast.createTrack("video");

	// Open the group and write one frame, but leave it open (in-progress).
	const group0 = producer.appendGroup();
	group0.writeFrame({ payload: enc.encode("alpha"), timestamp: Timestamp.fromMillis(10) });

	const remote = client.consume(Path.from("test"));
	const fetched = await remote.track("video").fetchGroup(0);

	const first = await fetched.readFrame();
	expect(dec.decode(first?.payload)).toBe("alpha");

	// Frames appended after the fetch started must still stream through, not be truncated.
	group0.writeFrame({ payload: enc.encode("beta"), timestamp: Timestamp.fromMillis(15) });
	const second = await fetched.readFrame();
	expect(dec.decode(second?.payload)).toBe("beta");
	expect(second?.timestamp.asMillis()).toBe(15);

	group0.close();
	expect(await fetched.readFrame()).toBeUndefined();

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: ietf fetch group is unsupported", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18);
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const remote = client.consume(Path.from("test"));
	await expect(remote.track("video").fetchGroup(0)).rejects.toThrow("fetch group is not supported for moq-transport");

	remote.close();
	client.close();
	server.close();
});

test("integration: ietf draft-14", async () => {
	await runPublishSubscribeFlow("", Ietf.Version.DRAFT_14);
});

test("integration: ietf draft-15", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_15);
});

test("integration: ietf draft-16", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_16);
});

test("integration: ietf draft-17", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_17);
});

test("integration: ietf draft-18", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_18);
});

test("integration: ietf draft-19", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_19);
});

// consume(path) dedupes per path: repeat calls for a still-live path share one reference-counted
// broadcast, so it stays live until every handle closes and a closed path re-consumes fresh.
async function runConsumeDedup(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version, publish: origin.consume() }),
	]);

	// Two handles to the same path share one broadcast: closing the first leaves it live...
	const first = client.consume(Path.from("shared"));
	const second = client.consume(Path.from("shared"));
	first.close();
	expect(first.closed.peek()).toBeUndefined();
	expect(second.closed.peek()).toBeUndefined();

	// ...and closing the last handle closes the shared broadcast (both handles observe it).
	second.close();
	expect(first.closed.peek()).toBeDefined();
	expect(second.closed.peek()).toBeDefined();

	// A different path is independent: a lone handle closes the broadcast immediately.
	const other = client.consume(Path.from("other"));
	other.close();
	expect(other.closed.peek()).toBeDefined();

	// Once closed, the path re-consumes fresh (a new live handle).
	const third = client.consume(Path.from("shared"));
	expect(third.closed.peek()).toBeUndefined();
	third.close();

	client.close();
	server.close();
}

async function waitUntil(predicate: () => boolean): Promise<void> {
	for (let i = 0; i < 200; i++) {
		if (predicate()) return;
		await sleep(5);
	}
	throw new Error("condition not met within timeout");
}

// Closing the last subscriber to a track tears the wire subscription down, so the publisher stops
// serving it (the muted-watch-tile case in #2355) instead of sending groups to a reader that left.
async function runSubscriberTeardown(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version, publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");
	video.writeString("hello");

	const remote = client.consume(Path.from("test"));
	const sub = remote.track("video").subscribe();
	expect(await sub.readString()).toBe("hello");

	// The publisher now has a live downstream reader for the track.
	await waitUntil(() => video.used.peek());

	// Closing the only subscriber must tear the wire subscription down, so demand drops on the
	// publisher rather than the relay serving groups to nobody.
	sub.close();
	await waitUntil(() => !video.used.peek());

	broadcast.close();
	remote.close();
	client.close();
	server.close();
}

test("integration: lite subscriber teardown on last unsubscribe", async () => {
	await runSubscriberTeardown(Lite.ALPN_06_WIP);
});

test("integration: ietf subscriber teardown on last unsubscribe", async () => {
	await runSubscriberTeardown(Ietf.ALPN.DRAFT_17);
});

// Draft-14 sends an explicit Unsubscribe after the demand loop breaks; exercise that path too.
// Uses a dynamic serve (draft-14 doesn't complete SUBSCRIBE_OK for a statically inserted track).
test("integration: ietf draft-14 subscriber teardown on last unsubscribe", async () => {
	const pair = createMockTransportPair("");
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14, publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));

	// Serve dynamically, keeping the served producer so we can watch its demand.
	let served: TrackProducer | undefined;
	const serving = (async () => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			served = req.accept();
			served.writeString("hello");
		}
	})();

	// Draft-14 only completes SUBSCRIBE_OK once the session is warmed by an announce round-trip.
	const announced = client.announced();
	await announced.next();

	const remote = client.consume(Path.from("test"));
	const sub = remote.track("video").subscribe();
	expect(await sub.readString()).toBe("hello");
	await waitUntil(() => served?.used.peek() === true);

	// Closing the subscriber sends Unsubscribe and tears the subscription down, so demand drops.
	sub.close();
	await waitUntil(() => served?.used.peek() === false);

	broadcast.close();
	await serving;
	announced.close();
	remote.close();
	client.close();
	server.close();
});

// A fetched group can stay open indefinitely (a catalog track, a JSON stream), so abandoning the
// fetch must cancel the FETCH stream rather than wait for a stream end that never comes.
test("integration: lite fetch teardown when the reader abandons an open group", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");
	const group = video.appendGroup(); // deliberately left open: an indefinite group.
	group.writeString("hello");

	const remote = client.consume(Path.from("test"));
	const fetched = await remote.track("video").fetchGroup(group.sequence);
	expect(await fetched.readString()).toBe("hello");

	// The publisher is now serving the still-open group.
	await waitUntil(() => group.used.peek());

	// Abandoning the fetch cancels the FETCH stream, so the publisher stops serving instead of
	// pumping an open group to a reader that left.
	fetched.close();
	await waitUntil(() => !group.used.peek());

	group.close();
	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

// Older drafts have no TRACK stream or SUBSCRIBE_UPDATE, so the demand loop must still tear the
// subscription down through the plain stream-close path.
test("integration: lite draft-01 subscriber teardown on last unsubscribe", async () => {
	await runSubscriberTeardown("", Lite.Version.DRAFT_01);
});

// Two subscribers to one track dedupe onto a single wire subscription: closing one keeps it alive
// for the other, and only the last close tears it down.
test("integration: lite fan-out keeps the upstream until the last subscriber leaves", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");
	video.writeString("hello");

	const remote = client.consume(Path.from("test"));
	const a = remote.track("video").subscribe();
	const b = remote.track("video").subscribe();
	expect(await a.readString()).toBe("hello");
	expect(await b.readString()).toBe("hello");
	await waitUntil(() => video.used.peek());

	// Closing one leaves the shared upstream serving the other.
	a.close();
	video.writeString("more");
	expect(await b.readString()).toBe("more");
	expect(video.used.peek()).toBe(true);

	// The last close tears it down.
	b.close();
	await waitUntil(() => !video.used.peek());

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

// Repeated subscribe/unsubscribe cycles must each tear down and re-open cleanly (the 40-toggle
// scenario in the issue), never wedging the shared cache or leaking a subscription.
test("integration: lite re-subscribe re-opens the upstream after each teardown", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");

	const remote = client.consume(Path.from("test"));

	for (let i = 0; i < 8; i++) {
		video.writeString(`hello-${i}`);
		const sub = remote.track("video").subscribe();
		expect(await sub.readString()).toBe(`hello-${i}`);
		await waitUntil(() => video.used.peek());

		sub.close();
		await waitUntil(() => !video.used.peek());
	}

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

// Coalesced fetches of one open group share a single FETCH stream: closing one keeps it flowing
// for the other, and only the last abandon cancels it.
test("integration: lite coalesced fetch stays until every reader abandons the open group", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");
	const group = video.appendGroup(); // open
	group.writeString("hello");

	const remote = client.consume(Path.from("test"));
	const f1 = await remote.track("video").fetchGroup(group.sequence);
	const f2 = await remote.track("video").fetchGroup(group.sequence);
	expect(await f1.readString()).toBe("hello");
	expect(await f2.readString()).toBe("hello");
	await waitUntil(() => group.used.peek());

	// Closing one coalesced reader keeps the shared FETCH flowing for the other.
	f1.close();
	group.writeString("more");
	expect(await f2.readString()).toBe("more");
	expect(group.used.peek()).toBe(true);

	// The last abandon cancels the FETCH.
	f2.close();
	await waitUntil(() => !group.used.peek());

	group.close();
	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

// A finite group must still deliver every frame and end cleanly (the demand watch must not disturb
// normal completion), exercising the per-frame loop many times.
test("integration: lite fetch delivers every frame of a finite multi-frame group", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const video = broadcast.createTrack("video");
	const group = video.appendGroup();
	const count = 50;
	for (let i = 0; i < count; i++) group.writeString(`f${i}`);
	group.close(); // finite

	const remote = client.consume(Path.from("test"));
	const fetched = await remote.track("video").fetchGroup(group.sequence);
	for (let i = 0; i < count; i++) {
		expect(await fetched.readString()).toBe(`f${i}`);
	}
	// Every frame read, then a clean end.
	expect(await fetched.readString()).toBeUndefined();

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite consume dedup", async () => {
	await runConsumeDedup(Lite.ALPN_05);
});

test("integration: ietf consume dedup", async () => {
	await runConsumeDedup(Ietf.ALPN.DRAFT_17);
});

// Drafts 14-16 multiplex both directions over one control stream. A subscribe must resolve when it
// races an inbound announce, without warming the session by reading that announce first.
async function runSubscribeWithoutWarmup(version: number) {
	const pair = createMockTransportPair("");
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version, publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const serving = (async () => {
		const req = await broadcast.requested();
		if (req) req.accept().writeString("hello");
	})();

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();
	const data = await Promise.race([
		track.readString(),
		new Promise<never>((_, reject) =>
			setTimeout(() => reject(new Error("timed out waiting for SUBSCRIBE_OK")), 2000),
		),
	]);
	expect(data).toBe("hello");

	broadcast.close();
	await serving;
	remote.close();
	client.close();
	server.close();
}

test("integration: ietf draft-14 subscribe without announce warmup", async () => {
	await runSubscribeWithoutWarmup(Ietf.Version.DRAFT_14);
});

test("integration: ietf draft-15 subscribe without announce warmup", async () => {
	await runSubscribeWithoutWarmup(Ietf.Version.DRAFT_15);
});

test("integration: ietf draft-16 subscribe without announce warmup", async () => {
	await runSubscribeWithoutWarmup(Ietf.Version.DRAFT_16);
});

test("integration: subscribe to non-existent broadcast", async () => {
	const pair = createMockTransportPair("");
	const origin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14, publish: origin.consume() }),
	]);

	// Client tries to consume a broadcast that nobody is publishing
	const remote = client.consume(Path.from("nonexistent"));
	const track = remote.subscribe("video");

	// Reading should eventually error since the broadcast doesn't exist
	await expect(
		(async () => {
			await track.readString();
		})(),
	).rejects.toThrow();

	client.close();
	server.close();
});

// Resolves once `signal` satisfies `pred`, returning the matching value.
async function waitFor<T>(signal: Getter<T>, pred: (value: T) => boolean): Promise<T> {
	for (;;) {
		const value = signal.peek();
		if (pred(value)) return value;
		await signal.changed();
	}
}

test("integration: announcedBroadcast waits for a late publisher", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	// Serves every requested track with `payload`, until the broadcast closes.
	const serve = async (broadcast: BroadcastProducer, payload: string) => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			req.accept().writeString(payload);
		}
	};

	// Nobody publishes this path yet. A blind consume would be reset (see the
	// "subscribe to non-existent broadcast" test); the handle just stays offline.
	const watched = client.announcedBroadcast(Path.from("late"));
	await sleep(50);
	expect(watched.active.peek()).toBeUndefined();

	// The publisher arrives afterwards.
	const first = origin.publish(Path.from("late"));
	const servingFirst = serve(first, "hello");

	const active = await waitFor(watched.active, (b) => b !== undefined);
	if (!active) throw new Error("expected an active broadcast");
	expect(await active.subscribe("video").readString()).toBe("hello");

	// It goes away.
	first.close();
	await servingFirst;
	await waitFor(watched.active, (b) => b === undefined);

	// And comes back under the same name: a fresh consumer, not the dead one.
	const second = origin.publish(Path.from("late"));
	const servingSecond = serve(second, "world");

	const republished = await waitFor(watched.active, (b) => b !== undefined);
	if (!republished) throw new Error("expected a republished broadcast");
	expect(republished).not.toBe(active);
	expect(await republished.subscribe("video").readString()).toBe("world");

	// Closing the handle releases the broadcast it held.
	watched.close();
	expect(watched.active.peek()).toBeUndefined();
	expect(republished.closed.peek()).not.toBeUndefined();

	second.close();
	await servingSecond;
	client.close();
	server.close();
});

test("integration: announcedBroadcast consumes blind without discovery", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, discovery: false }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const broadcast = origin.publish(Path.from("test"));
	const serving = (async () => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			req.accept().writeString("blind");
		}
	})();

	// No announcement ever arrives, so waiting for one would hang. Subscribe anyway.
	const watched = client.announcedBroadcast(Path.from("test"));
	const active = await waitFor(watched.active, (b) => b !== undefined);
	if (!active) throw new Error("expected an active broadcast");
	expect(await active.subscribe("video").readString()).toBe("blind");

	watched.close();
	broadcast.close();
	await serving;
	client.close();
	server.close();
});

test("integration: a republish is not served from the previous generation's cache", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const serve = async (broadcast: BroadcastProducer, payload: string) => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			req.accept().writeString(payload);
		}
	};

	const first = origin.publish(Path.from("shared"));
	const servingFirst = serve(first, "old");

	const watched = client.announcedBroadcast(Path.from("shared"));
	const active = await waitFor(watched.active, (b) => b !== undefined);
	if (!active) throw new Error("expected an active broadcast");
	expect(await active.subscribe("video").readString()).toBe("old");

	// A second holder of the same path, which is what makes the cache reachable: consumed
	// broadcasts are reference-counted, so the handle closing its own copy below does not
	// release the shared one.
	const bystander = client.consume(Path.from("shared"));

	first.close();
	await servingFirst;
	await waitFor(watched.active, (b) => b === undefined);

	// The republish must subscribe fresh. Cloning the cached entry would resolve the previous
	// generation's tracks, which the wire has already reset.
	const second = origin.publish(Path.from("shared"));
	const servingSecond = serve(second, "new");

	const republished = await waitFor(watched.active, (b) => b !== undefined);
	if (!republished) throw new Error("expected a republished broadcast");
	expect(await republished.subscribe("video").readString()).toBe("new");

	bystander.close();
	watched.close();
	second.close();
	await servingSecond;
	client.close();
	server.close();
});

test("integration: a blind handle picks up a publisher that arrives late", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, discovery: false }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	// Without discovery there is no announcement to wait for, so the handle consumes blind.
	const watched = client.announcedBroadcast(Path.from("later"));
	const blind = await waitFor(watched.active, (b) => b !== undefined);
	if (!blind) throw new Error("expected a blind consumer");

	// Nobody publishes the path yet, so a subscribe is how the caller finds out. That kills the
	// track, not the handle: a consumed broadcast is scoped to the path, not to one publisher.
	await expect(blind.subscribe("video").readString()).rejects.toThrow();
	expect(watched.active.peek()).toBe(blind);

	// So a subscribe made after the publisher finally shows up still works, on the same handle.
	const producer = origin.publish(Path.from("later"));
	const serving = (async () => {
		for (;;) {
			const req = await producer.requested();
			if (!req) break;
			req.accept().writeString("late");
		}
	})();

	expect(await blind.subscribe("video").readString()).toBe("late");

	watched.close();
	producer.close();
	await serving;
	client.close();
	server.close();
});

test("integration: a blind handle goes offline when the session dies", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, discovery: false }),
		accept(pair.server, url, { publish: origin.consume() }),
	]);

	const watched = client.announcedBroadcast(Path.from("whatever"));
	await waitFor(watched.active, (b) => b !== undefined);

	// The gated path goes offline when the announcement stream ends with the session. There is
	// no stream here, so the session itself is what has to clear it.
	server.close();
	client.close();
	await waitFor(watched.active, (b) => b === undefined);

	watched.close();
});

// The handle and the consume-cache eviction are protocol-agnostic, but their implementations
// are not: each subscriber resolves announcements its own way. These mirror the lite cases.
test("integration: ietf blind handle picks up a publisher that arrives late", async () => {
	const pair = createMockTransportPair("");
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, discovery: false }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14, publish: origin.consume() }),
	]);

	const watched = client.announcedBroadcast(Path.from("later"));
	const blind = await waitFor(watched.active, (b) => b !== undefined);
	if (!blind) throw new Error("expected a blind consumer");

	// Rejects with 404 rather than resetting the whole handle.
	await expect(blind.subscribe("video").readString()).rejects.toThrow();
	expect(watched.active.peek()).toBe(blind);

	const producer = origin.publish(Path.from("later"));
	const serving = (async () => {
		for (;;) {
			const req = await producer.requested();
			if (!req) break;
			req.accept().writeString("ietf-late");
		}
	})();

	expect(await blind.subscribe("video").readString()).toBe("ietf-late");

	watched.close();
	producer.close();
	await serving;
	client.close();
	server.close();
});

// ---------------------------------------------------------------------------
// Origin-fed sessions: the `subscribe` option end to end.
// ---------------------------------------------------------------------------

/** Poll until `pred` holds, so a regression fails the test instead of hanging it. */
async function until(pred: () => boolean): Promise<void> {
	for (let i = 0; i < 500; i++) {
		if (pred()) return;
		await sleep(1);
	}
	throw new Error("timed out waiting for condition");
}

async function runOriginFlow(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);
	const serverOrigin = new OriginProducer();
	const clientOrigin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, subscribe: clientOrigin }),
		accept(pair.server, url, { version, publish: serverOrigin.consume() }),
	]);

	// The server publishes into its origin; the wire announces it.
	const broadcast = serverOrigin.publish(Path.from("test"));
	const serving = (async () => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			if (req.name !== "video") {
				req.reject(new Error(`unexpected track: ${req.name}`));
				continue;
			}
			req.accept().writeString("hello");
		}
	})();

	// The announcement lands in the client's origin.
	const reader = clientOrigin.consume();
	const announced = reader.announced();
	expect(await announced.next()).toEqual({ path: Path.from("test"), active: true });

	// Consuming through the origin reaches the wire.
	const remote = routed(reader, Path.from("test"));
	if (!remote) throw new Error("expected the origin to route the broadcast");
	const track = remote.track("video").subscribe();
	expect(await track.readString()).toBe("hello");

	// Unpublishing retracts the entry over the wire and out of the origin.
	broadcast.close();
	expect(await announced.next()).toEqual({ path: Path.from("test"), active: false });
	await until(() => !reader.routes(Path.from("test")));

	await serving;
	track.close();
	remote.close();
	announced.close();
	client.close();
	server.close();
	serverOrigin.close();
	clientOrigin.close();
}

test("origin: discovers, consumes, and retracts over lite", async () => {
	await runOriginFlow(Lite.ALPN_05);
});

test("origin: discovers, consumes, and retracts over ietf", async () => {
	await runOriginFlow("", Ietf.Version.DRAFT_14);
});

test("origin: remote entries retract when the session dies, local ones survive", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05);
	const serverOrigin = new OriginProducer();
	const clientOrigin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, subscribe: clientOrigin }),
		accept(pair.server, url, { publish: serverOrigin.consume() }),
	]);

	serverOrigin.publish(Path.from("remote"));
	const mine = clientOrigin.publish(Path.from("mine"));

	const reader = clientOrigin.consume();
	await until(() => reader.routes(Path.from("remote")));

	client.close();
	server.close();

	// The session that fed the entry is gone, so the entry goes with it.
	await until(() => !reader.routes(Path.from("remote")));

	// The local publish is not the session's to take.
	const local = routed(reader, Path.from("mine"));
	expect(local).toBeDefined();
	local?.close();

	mine.close();
	serverOrigin.close();
	clientOrigin.close();
});

test("origin: one origin on both directions consumes locally and never echoes", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05);

	// The client routes both directions through one origin, the FFI default shape.
	const shared = new OriginProducer();
	// The server feeds what the client announces into its own origin, so an echo would land here.
	const serverSees = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, publish: shared.consume(), subscribe: shared }),
		accept(pair.server, url, { publish: serverSees.consume(), subscribe: serverSees }),
	]);

	// The server announces a broadcast; it lands in the shared origin as a remote entry.
	{
		const remote = serverSees.publish(Path.from("from-server"));
		const reader = shared.consume();
		await until(() => reader.routes(Path.from("from-server")));
		remote.close();
	}

	// The client publishes; consuming its own path through the shared origin is local, no wire.
	const mine = shared.publish(Path.from("from-client"));
	mine.createTrack("chat");
	const reader = shared.consume();
	const loopback = routed(reader, Path.from("from-client"));
	if (!loopback) throw new Error("expected a local route");
	const track = loopback.subscribe("chat");
	expect(track).toBeDefined();
	track.close();
	loopback.close();

	// The server sees the client's broadcast once, as its own remote entry.
	const serverReader = serverSees.consume();
	await until(() => serverReader.routes(Path.from("from-client")));

	// The critical part: the client must NOT re-announce "from-server" back. If it did, the
	// server's forwarder would insert it as a remote entry in serverSees. Give the wire a
	// moment, then check the only remote entry the server has is the client's own broadcast.
	await sleep(50);
	expect(serverReader.routes(Path.from("from-server"))).toBe(false);

	mine.close();
	client.close();
	server.close();
	shared.close();
	serverSees.close();
});

test("origin: a request resolves blind on a relay without discovery", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05);
	const serverOrigin = new OriginProducer();
	const clientOrigin = new OriginProducer();

	const [client, server] = await Promise.all([
		// The client believes the relay lacks discovery, so no announce stream opens.
		connect(url, { transport: pair.client, subscribe: clientOrigin, discovery: false }),
		accept(pair.server, url, { publish: serverOrigin.consume() }),
	]);

	const broadcast = serverOrigin.publish(Path.from("blind"));
	const serving = (async () => {
		for (;;) {
			const req = await broadcast.requested();
			if (!req) break;
			req.accept().writeString("found you");
		}
	})();

	const reader = clientOrigin.consume();
	expect(reader.discovery.peek()).toBe(false);

	// Nothing announced, so the table stays empty; a request is the only way through.
	expect(reader.routes(Path.from("blind"))).toBe(false);

	const request = reader.request(Path.from("blind"));
	await until(() => request.active.peek() !== undefined);

	const front = request.active.peek();
	const track = front?.track("chat").subscribe();
	if (!track) throw new Error("expected a track");
	expect(await track.readString()).toBe("found you");

	track.close();
	request.close();
	broadcast.close();
	await serving;
	client.close();
	server.close();
	serverOrigin.close();
	clientOrigin.close();
});

test("origin: a request is re-answered by the next session", async () => {
	const original = globalThis.WebTransport;
	const reconnectUrl = new URL("https://example.com/re-request");

	const servers: { session: { close: () => void }; origin: OriginProducer }[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const serverOrigin = new OriginProducer();
		void accept(pair.server, reconnectUrl, { publish: serverOrigin.consume() }).then((session) => {
			servers.push({ session, origin: serverOrigin });
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const clientOrigin = new OriginProducer();
	const reload = new Reload({
		enabled: true,
		url: reconnectUrl,
		websocket: { enabled: false },
		delay: { initial: 10, multiplier: 1, max: 10 },
		subscribe: clientOrigin,
	});

	const request = clientOrigin.request(Path.from("standing"));

	try {
		await until(() => request.active.peek() !== undefined);
		const first = request.active.peek();

		// The answering session dies: the answer is withdrawn, not the request.
		servers[0]?.session.close();
		await until(() => request.active.peek() === undefined);

		// The next session answers the same standing request.
		await until(() => request.active.peek() !== undefined);
		expect(request.active.peek()).not.toBe(first);
	} finally {
		request.close();
		reload.close();
		clientOrigin.close();
		globalThis.WebTransport = original;
	}
});

test("origin: a reactive handle follows announcements, republishes, and reconnect gaps", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05);
	const serverOrigin = new OriginProducer();
	const clientOrigin = new OriginProducer();

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client, subscribe: clientOrigin }),
		accept(pair.server, url, { publish: serverOrigin.consume() }),
	]);

	const watch = new Announce.Broadcast({ origin: clientOrigin, path: Path.from("show") });

	// Nothing published yet: the handle waits instead of subscribing blind.
	for (let i = 0; i < 5; i++) await sleep(1);
	expect(watch.active.peek()).toBeUndefined();

	// The publish resolves it through the wire.
	const first = serverOrigin.publish(Path.from("show"));
	await until(() => watch.active.peek() !== undefined);
	const held = watch.active.peek();

	// A republish must swap the handle to the new broadcast, not cling to the dead one.
	const second = serverOrigin.publish(Path.from("show"));
	await until(() => watch.active.peek() !== undefined && watch.active.peek() !== held);

	// Unpublishing takes it offline.
	second.close();
	first.close();
	await until(() => watch.active.peek() === undefined);

	watch.close();
	client.close();
	server.close();
	serverOrigin.close();
	clientOrigin.close();
});

test("origin: overlapping sessions carrying one path fail over", async () => {
	// Two live relays announce the same broadcast into one origin, the redundant-relay
	// shape (and the GOAWAY drain shape, where old and new sessions briefly overlap).
	const clientOrigin = new OriginProducer();

	const setup = async () => {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const serverOrigin = new OriginProducer();
		const [client, server] = await Promise.all([
			connect(url, { transport: pair.client, subscribe: clientOrigin }),
			accept(pair.server, url, { publish: serverOrigin.consume() }),
		]);
		const broadcast = serverOrigin.publish(Path.from("redundant"));
		const serving = (async () => {
			for (;;) {
				const req = await broadcast.requested();
				if (!req) break;
				req.accept().writeString("still here");
			}
		})();
		return { client, server, serverOrigin, broadcast, serving };
	};

	const first = await setup();
	const second = await setup();

	const reader = clientOrigin.consume();
	await until(() => reader.routes(Path.from("redundant")));

	// The newer session dies; the older one still carries the path and must keep serving.
	second.client.close();
	second.server.close();
	await sleep(50);

	const remote = routed(reader, Path.from("redundant"));
	if (!remote) throw new Error("route black-holed despite a live session");
	const track = remote.track("chat").subscribe();
	expect(await track.readString()).toBe("still here");

	track.close();
	remote.close();
	first.broadcast.close();
	await first.serving;
	first.client.close();
	first.server.close();
	first.serverOrigin.close();
	second.serverOrigin.close();
	second.broadcast.close();
	clientOrigin.close();
});

test("origin: a standby session re-answers a request when the answerer dies", async () => {
	const clientOrigin = new OriginProducer();

	const setup = async (payload: string) => {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const serverOrigin = new OriginProducer();
		const [client, server] = await Promise.all([
			// No discovery: requests are the only way through.
			connect(url, { transport: pair.client, subscribe: clientOrigin, discovery: false }),
			accept(pair.server, url, { publish: serverOrigin.consume() }),
		]);
		const broadcast = serverOrigin.publish(Path.from("blind"));
		const serving = (async () => {
			for (;;) {
				const req = await broadcast.requested();
				if (!req) break;
				req.accept().writeString(payload);
			}
		})();
		return { client, server, serverOrigin, broadcast, serving };
	};

	// The first session answers the standing request; the second attaches as a standby.
	const request = clientOrigin.request(Path.from("blind"));
	const answerer = await setup("from answerer");
	await until(() => request.active.peek() !== undefined);
	const standby = await setup("from standby");

	// The answering session dies while the standby stays connected: the withdrawal must
	// wake the standby's serving loop, not wait for a brand-new session. The handoff can
	// complete within one scheduler tick, so assert the front changed rather than racing
	// to observe the vacant slot.
	const before = request.active.peek();
	answerer.client.close();
	answerer.server.close();
	await until(() => request.active.peek() !== undefined && request.active.peek() !== before);

	const front = request.active.peek();
	const track = front?.track("chat").subscribe();
	if (!track) throw new Error("expected a track from the standby");
	expect(await track.readString()).toBe("from standby");

	track.close();
	request.close();
	standby.broadcast.close();
	await standby.serving;
	standby.client.close();
	standby.server.close();
	standby.serverOrigin.close();
	answerer.serverOrigin.close();
	answerer.broadcast.close();
	clientOrigin.close();
});
