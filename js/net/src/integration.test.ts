import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Producer as TrackProducer } from "./track.ts";

const url = new URL("https://localhost:4443/test");

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

async function runPublishSubscribeFlow(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, version !== undefined ? { version } : undefined),
	]);

	// Server publishes a broadcast
	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
	const prefixedBroadcast = new BroadcastProducer();
	server.publish(Path.from("root/child"), prefixedBroadcast);

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

test("integration: lite draft-06", async () => {
	// Exercises announce ids: every active assigns an ordinal on the wire.
	await runPublishSubscribeFlow(Lite.ALPN_06_WIP);
});

test("integration: lite draft-06 announce lifecycle", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	// Announced before the client asks, so it can ride the initial set.
	const first = new BroadcastProducer();
	server.publish(Path.from("first"), first);

	const announced = client.announced();
	let entry = await announced.next();
	if (!entry) throw new Error("expected announce");
	expect(entry.path).toBe("first" as Path.Valid);
	expect(entry.active).toBe(true);

	// A live announce.
	const second = new BroadcastProducer();
	server.publish(Path.from("second"), second);
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
	const secondAgain = new BroadcastProducer();
	server.publish(Path.from("second"), secondAgain);
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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	// A static track fans datagrams out to whoever subscribes.
	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

test("integration: lite draft-05 fetches a cached group", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");
	server.publish(Path.from("test"), broadcast);

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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");
	server.publish(Path.from("test"), broadcast);

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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("video");
	server.publish(Path.from("test"), broadcast);

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

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

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
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, version !== undefined ? { version } : undefined),
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
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, version !== undefined ? { version } : undefined),
	]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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
	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14 }),
	]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);

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
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
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

test("integration: subscribe to non-existent broadcast", async () => {
	const pair = createMockTransportPair("");

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14 }),
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
