import { expect, jest, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { Once } from "@moq/signals";
import type * as announce from "../announced.ts";
import * as Epoch from "../epoch.ts";
import { ProtocolViolation, StreamCode, Stream as StreamError } from "../error.ts";
import { type Hop, HopSchema, UNKNOWN_HOP } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Tail } from "../tail.ts";
import { Timescale, Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import * as demandUtil from "../util/demand.ts";
import type * as Cluster from "./cluster.ts";
import { Connection } from "./connection.ts";
import { ObjectDatagram } from "./datagram.ts";
import { encodeObjectExtensions, type GroupFlags, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import type { Properties } from "./properties.ts";
import { PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_18;

/** How long to wait for a stream before calling it absent. */
const STREAM_WAIT = 500;

/**
 * Accept the next stream the subscriber opens, or give up rather than hang forever.
 *
 * Reads the queue directly instead of racing {@link Stream.accept}, whose pending read
 * would keep the reader locked after the race resolves and could swallow a later stream.
 */
async function nextStream(transport: WebTransport, version: IetfVersion = VERSION): Promise<Stream | undefined> {
	const reader =
		transport.incomingBidirectionalStreams.getReader() as ReadableStreamDefaultReader<WebTransportBidirectionalStream>;

	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const next = await Promise.race([
			reader.read(),
			new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), STREAM_WAIT);
			}),
		]);

		if (!next || next.done) return undefined;
		return new Stream({ readable: next.value.readable, writable: next.value.writable, version });
	} finally {
		clearTimeout(timer);
		reader.releaseLock();
	}
}

/**
 * Every peer is asked, whatever it declared. A peer with nothing to advertise answers
 * with an empty set, which costs one stream, and a peer that only answers when asked is
 * the one that would otherwise never be discovered.
 */
test("every peer is asked", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);

	const subscriber = new Subscriber({ session });
	subscriber.announced();

	expect(await nextStream(pair.client)).toBeDefined();
});

/**
 * The other half of discovery: a peer that tells us unasked. Asking must not make us deaf
 * to a PUBLISH_NAMESPACE that arrives on its own stream instead.
 */
test("an unsolicited announcement lands", async () => {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();

	// The question we asked, which this peer never answers.
	expect(await nextStream(pair.client)).toBeDefined();

	// What the connection dispatch does when a PUBLISH_NAMESPACE arrives instead.
	const stream = await Stream.open(pair.server, { version: version });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("surprise") }),
		stream,
	);

	const next = await announced.next();
	expect(next?.prefix).toBe(Path.from("surprise"));
	expect(next?.kind).toBe("start");

	// The handler holds the request open until the peer drops it, and withdraws the
	// namespace on the way out.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.writer.close();
	await peer.writer.closed;
	// A second advertisement is a processing barrier: the FIN must not retract the first.
	const second = await Stream.open(pair.server, { version: version });
	const other = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 2n, trackNamespace: Path.from("sentinel") }),
		second,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("sentinel"), kind: "start" });
	peer.close();
	await handler;
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("surprise"),
		kind: "end",
	});
	const secondPeer = await nextStream(pair.client);
	if (!secondPeer) throw new Error("missing sentinel stream");
	secondPeer.close();
	await other;
});

/**
 * A session without the Cluster extension names no publisher, so its advertisement carries
 * only the anonymous mark: identity is the epoch's job, and nothing goes in front of the 0.
 */
test("an advertisement with no path is anonymous", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true) });
	const announced = subscriber.announced();
	expect(await nextStream(pair.client)).toBeDefined();

	const stream = await Stream.open(pair.server, { version: VERSION });
	void subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("legacy") }),
		stream,
	);
	expect((await announced.next())?.route.hops).toEqual([UNKNOWN_HOP]);
});

/**
 * Answer the SUBSCRIBE_NAMESPACE the subscriber just opened, then hand back its stream so
 * the test can feed inline NAMESPACE entries down it.
 */
async function acceptSubscribeNamespace(transport: WebTransport, version: IetfVersion = VERSION): Promise<Stream> {
	const stream = await nextStream(transport, version);
	if (!stream) throw new Error("no SUBSCRIBE_NAMESPACE was sent");

	// Drain the request, then answer it.
	await stream.reader.u53();
	await SubscribeNamespace.decode(stream.reader, version);
	await stream.writer.u53(RequestOk.id);
	await new RequestOk().encode(stream.writer, version);

	return stream;
}

/** Advertise `path` inline on a SUBSCRIBE_NAMESPACE stream. */
async function inlineNamespace(
	stream: Stream,
	path: Path.Valid,
	cluster?: Cluster.Advert,
	version: IetfVersion = VERSION,
): Promise<void> {
	await stream.writer.u53(SubscribeNamespaceEntry.id);
	await new SubscribeNamespaceEntry({ suffix: path, params: cluster && { cluster } }).encode(stream.writer, version);
}

/**
 * Advertise a path nothing else uses and wait for it, which proves the entries written
 * before it on the same stream have been read. Announcing an already-announced path is
 * silent by design, so it cannot be waited on directly.
 */
async function syncInline(stream: Stream, announced: announce.Consumer, cluster?: Cluster.Advert): Promise<void> {
	await inlineNamespace(stream, Path.from("sentinel"), cluster);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("sentinel"),
		kind: "start",
	});
}

/**
 * A peer may advertise one namespace both ways on a session: an unsolicited
 * PUBLISH_NAMESPACE and an inline NAMESPACE answering our own SUBSCRIBE_NAMESPACE are two
 * messages about one source, and the MoQ Solicit draft requires us to tolerate it.
 *
 * The unsolicited request ending must not retract what the subscription still holds, or a
 * watcher drops a broadcast that is still being published.
 */
test("an announcement survives the first of its two sources ending", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Unsolicited first, then the same path inline on the subscription.
	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("both") }),
		request,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("both"), kind: "start" });

	await inlineNamespace(subscription, Path.from("both"));
	await syncInline(subscription, announced);

	// The unsolicited request goes away. The subscription still advertises the path, so
	// nothing has been withdrawn and there is nothing for the consumer to hear.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await handler;

	const next = await Promise.race([
		announced.next(),
		new Promise<"nothing">((resolve) => setTimeout(() => resolve("nothing"), 250)),
	]);
	expect(next).toBe("nothing");
});

/**
 * The mirror of the above, and the reason the count exists rather than a flag: once the
 * last source goes, the path really is gone and consumers have to hear it.
 */
test("an announcement ends once its last source does", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("both") }),
		request,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("both"), kind: "start" });

	await inlineNamespace(subscription, Path.from("both"));
	await syncInline(subscription, announced);

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await handler;

	// Now the subscription drops it too, which is the last reference.
	await subscription.writer.u53(SubscribeNamespaceEntryDone.id);
	await new SubscribeNamespaceEntryDone({ suffix: Path.from("both") }).encode(subscription.writer, VERSION);

	expect(await announced.next()).toMatchObject({ prefix: Path.from("both"), kind: "end" });
});

/**
 * A stream owns every advertisement it carried, so losing it retracts them: closing a
 * subscription withdraws nothing on the wire, since NAMESPACE_DONE is what does that.
 * Whatever is still live outlived its channel, and a count left behind would pin the path
 * for the rest of the session, for every other consumer too.
 */
test("a subscription that dies releases what it advertised", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	// Two feeds, so one outlives the stream that carries the advertisement.
	const doomed = subscriber.announced();
	const streamA = await acceptSubscribeNamespace(pair.client);
	const survivor = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	await inlineNamespace(streamA, Path.from("orphan"));
	expect(await doomed.next()).toMatchObject({ prefix: Path.from("orphan"), kind: "start" });
	expect(await survivor.next()).toMatchObject({ prefix: Path.from("orphan"), kind: "start" });

	// The stream that advertised it goes away without a NAMESPACE_DONE.
	streamA.writer.close();

	expect(await survivor.next()).toMatchObject({ prefix: Path.from("orphan"), kind: "end" });
});

/**
 * The read loop is not awaited when the local consumer closes first, and closing the
 * stream cancels the transport without discarding what the reader already buffered. An
 * entry that decodes after the teardown must not take a reference, or the path is pinned
 * for the session with nobody left to release it.
 */
test("an entry buffered past a consumer close does not pin the path", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Both entries land in one chunk, so the second is buffered while the first is being
	// delivered. Closing the consumer then races the loop that would attach it.
	await inlineNamespace(subscription, Path.from("first"));
	await inlineNamespace(subscription, Path.from("buffered"));

	announced.close();
	// The peer ends its half on the cancel, so the request leaves the wire.
	subscription.writer.close();

	// Whatever the loop managed to attach, the stream gave back on its way out.
	await new Promise((resolve) => setTimeout(resolve, 50));

	const fresh = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);
	const seeded = await Promise.race([
		fresh.next(),
		new Promise<"nothing">((resolve) => setTimeout(() => resolve("nothing"), 250)),
	]);
	expect(seeded).toBe("nothing");
});

/** The Hop IDs a cluster-negotiated session declared, ours first. */
const SELF: Hop = HopSchema.parse(7n);
const PEER: Hop = HopSchema.parse(9n);

/**
 * A peer that knows our Hop ID never advertises a path that already ran through us, so
 * this is the backstop for one that does not conform: subscribing via such a path would
 * route us back to ourselves, so the advertisement is dropped rather than announced.
 */
test("an inline NAMESPACE that looped back through us is dropped", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Ours coming back, then someone else's. Only the second is news.
	await inlineNamespace(subscription, Path.from("mine"), { hops: [SELF, PEER], cost: 0n });
	await syncInline(subscription, announced, { hops: [PEER], cost: 0n });
});

/**
 * An advertisement is updated in place, by re-sending it on the stream that carries it. One
 * that now loops back has re-parented onto a route we cannot subscribe over, so the path is
 * gone even though the message says active.
 */
test("an inline NAMESPACE that starts looping back is retracted", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	await inlineNamespace(subscription, Path.from("theirs"), { hops: [PEER], cost: 0n });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });

	await inlineNamespace(subscription, Path.from("theirs"), { hops: [SELF, PEER], cost: 0n });
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "end" });
});

/**
 * NAMESPACE has no REQUEST_UPDATE, so a peer reprices one by re-sending it. The repeat is
 * neither a duplicate nor a retraction: the stored route changes and consumers hear
 * `update`.
 */
test("a repeated NAMESPACE reprices in place", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	await inlineNamespace(subscription, Path.from("theirs"), { hops: [PEER], cost: 4n });
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("theirs"),
		kind: "start",
		route: { hops: [PEER], cost: 4n },
	});

	await inlineNamespace(subscription, Path.from("theirs"), { hops: [PEER], cost: 0n });
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("theirs"),
		kind: "update",
		route: { hops: [PEER], cost: 0n },
	});
});

/** The same rule on the other kind of advertisement, which is a request we can refuse. */
test("a PUBLISH_NAMESPACE that looped back through us is refused", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	await subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("mine"),
			params: { cluster: { hops: [SELF, PEER], cost: 0n } },
		}),
		request,
	);

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestError.id);
	const err = await RequestError.decode(peer.reader, VERSION);
	// UNINTERESTED, draft-22 section 16.11.2: stop offering us this namespace.
	expect(err.errorCode).toBe(0x20);

	// Refused, so nothing was announced: the sentinel is the first thing a consumer hears.
	await syncInline(subscription, announced, { hops: [PEER], cost: 0n });
});

/**
 * The cluster draft requires closing the session over an advertisement missing its HOP_PATH,
 * not just the stream that carried it: a peer that broke the protocol once would otherwise
 * repeat it on the next SUBSCRIBE_NAMESPACE.
 */
test("a NAMESPACE missing its hop path closes the session", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	// The base form, which a negotiated session must never send.
	await subscription.writer.u53(SubscribeNamespaceEntry.id);
	await new SubscribeNamespaceEntry({ suffix: Path.from("nohops") }).encode(subscription.writer, VERSION);

	await Promise.race([
		pair.server.closed,
		new Promise((_resolve, reject) => setTimeout(() => reject(new Error("session stayed up")), STREAM_WAIT)),
	]);
});

test("a malformed PUBLISH_NAMESPACE update closes the session", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const control = await Stream.open(pair.server, { version: VERSION });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version: VERSION,
		client: false,
		cluster: { self: SELF, peer: PEER },
	});
	const logged = spyOn(console, "error").mockImplementation(() => void 0);

	try {
		const request = await Stream.open(pair.client, { version: VERSION });
		await request.writer.u53(PublishNamespace.id);
		await new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			params: { cluster: { hops: [PEER], cost: 0n } },
		}).encode(request.writer, VERSION);

		expect(await request.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(request.reader, VERSION);

		// The body promises a parameter block after the request ID but ends first.
		await request.writer.u53(PublishNamespaceUpdate.id);
		await request.writer.u16(1);
		await request.writer.u8(3);

		await Promise.race([
			pair.server.closed,
			new Promise((_resolve, reject) => setTimeout(() => reject(new Error("session stayed up")), STREAM_WAIT)),
		]);
	} finally {
		logged.mockRestore();
		connection.abort();
	}
});

/**
 * An advertisement is updated in place with REQUEST_UPDATE on the stream that carries it,
 * and each update is acknowledged. One re-parented onto a route through us is unusable,
 * so the announcement has to go even though the stream stays open, and a later clean
 * path on the same stream brings it back.
 */
test("a PUBLISH_NAMESPACE update that starts looping back is detached", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	// Published by 11, relayed by the peer. Every update keeps that publisher.
	const publisher = HopSchema.parse(11n);
	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			params: { cluster: { hops: [publisher, PEER], cost: 0n } },
		}),
		request,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);

	// The peer re-parents the namespace onto a route that runs back through us.
	await peer.writer.u53(PublishNamespaceUpdate.id);
	await new PublishNamespaceUpdate({ requestId: 3n, update: { hops: [publisher, SELF, PEER] } }).encode(
		peer.writer,
		VERSION,
	);

	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "end" });
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);

	// A clean path again, with the cost alongside: the path it lands on is the one held.
	await peer.writer.u53(PublishNamespaceUpdate.id);
	await new PublishNamespaceUpdate({ requestId: 5n, update: { hops: [publisher, PEER], cost: 0n } }).encode(
		peer.writer,
		VERSION,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);

	peer.close();
	await handler;
});

/**
 * REQUEST_UPDATE keeps an omitted parameter, so an explicit ROUTE_COST of 0 lands on the
 * path already held without disturbing the announcement. Consumers hear `update` with the
 * new cost; the stream ending is what retracts it.
 */
test("a PUBLISH_NAMESPACE repricing is acknowledged in place", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			params: { cluster: { hops: [PEER], cost: 4n } },
		}),
		request,
	);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("theirs"),
		kind: "start",
		route: { hops: [PEER], cost: 4n },
	});

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);

	await peer.writer.u53(PublishNamespaceUpdate.id);
	await new PublishNamespaceUpdate({ requestId: 3n, update: { cost: 0n } }).encode(peer.writer, VERSION);
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("theirs"),
		kind: "update",
		route: { hops: [PEER], cost: 0n },
	});

	// Still announced: the stream ending is what retracts it.
	peer.close();
	await handler;
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "end" });
});

/**
 * An update whose first Hop ID differs names a different publisher. It still updates the
 * advertisement in place, the stream stays open, and the path names the same broadcast, so
 * the next consume shares it.
 */
test("a PUBLISH_NAMESPACE update that changes the publisher applies in place", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			params: { cluster: { hops: [HopSchema.parse(11n), PEER], cost: 0n } },
		}),
		request,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);
	const held = subscriber.consume(Path.from("theirs"));

	await peer.writer.u53(PublishNamespaceUpdate.id);
	await new PublishNamespaceUpdate({ requestId: 3n, update: { hops: [HopSchema.parse(8n), PEER] } }).encode(
		peer.writer,
		VERSION,
	);
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);
	expect(await announced.next()).toMatchObject({
		prefix: Path.from("theirs"),
		kind: "update",
		route: { hops: [HopSchema.parse(8n), PEER] },
	});

	expect(subscriber.consume(Path.from("theirs")).closed).toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();

	// Still announced: the stream ending is what retracts it.
	peer.close();
	await handler;
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "end" });
});

test("a consumed broadcast refuses to list its tracks rather than claim it has none", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });
	const consumer = subscriber.consume(Path.from("theirs"));
	await expect(consumer.tracks().next()).rejects.toThrow("track listing requires moq-lite-07");
	// A later reader asks again, and is refused again.
	await expect(consumer.tracks().next()).rejects.toThrow("track listing requires moq-lite-07");
	consumer.close();
	subscriber.close();
});

/**
 * A second PUBLISH_NAMESPACE on the stream that already carries one is no longer an
 * update: it is the base draft's duplicate request, a protocol violation.
 */
test("a repeated PUBLISH_NAMESPACE is a protocol violation", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, quic: pair.server, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const advert = new PublishNamespace({
		requestId: 0n,
		trackNamespace: Path.from("theirs"),
		params: { cluster: { hops: [PEER], cost: 0n } },
	});
	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(advert, request);
	// A writer may yield a task before the rejection assertion below.
	void handler.catch(() => {});
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	await peer.writer.u53(PublishNamespace.id);
	await advert.encode(peer.writer, VERSION);

	// The malformed request can reject while the writer yields a browser task.
	await new Promise<void>((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
	await expect(handler).rejects.toThrow(ProtocolViolation);
	// The request closes the session itself, which takes the namespace subscription down too.
	await violationClosed(pair);
	await expect(announced.next()).rejects.toThrow();
});

/** The alias the group streams below are published on. */
const ALIAS = 9n;

/** Group flags for a plain subgroup stream: no extensions, no subgroup id, no properties. */
function groupFlags(firstObject: boolean): GroupFlags {
	return {
		hasExtensions: false,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject,
	};
}

/**
 * The objects of a subgroup stream, written by hand.
 *
 * `deltas` are the raw Object ID Deltas, which is the whole point: a publisher trimming a
 * group's head puts the first object's absolute id there, and nothing on our side will
 * encode that.
 */
function encodeObjects(deltas: number[]): Uint8Array {
	const bytes: number[] = [];
	for (const delta of deltas) {
		const payload = new TextEncoder().encode(`object ${delta}`);
		// Every field here is under 64, so each is a one-byte varint.
		bytes.push(delta, payload.byteLength, ...payload);
	}
	return new Uint8Array(bytes);
}

/**
 * A subscriber with one track subscribed and answered, which is what registers {@link ALIAS}
 * and lets a group stream naming it be handled.
 */
async function subscribeTrack({
	version = VERSION,
	properties = {},
	subscription,
}: {
	version?: IetfVersion;
	properties?: Properties;
	subscription?: track.Subscription;
} = {}): Promise<{ subscriber: Subscriber; track: track.Subscriber }> {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });

	const track = subscriber.consume(Path.from("room")).track("video").subscribe(subscription);

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("the subscriber never opened a subscribe stream");
	peer.reader.version = version;
	peer.writer.version = version;

	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, version);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ trackAlias: ALIAS, properties }).encode(peer.writer, version);

	return { subscriber, track };
}

// A refusal already ended the request: a reset back would tell the publisher we failed.
test("a refused SUBSCRIBE or SUBSCRIBE_NAMESPACE ends its stream cleanly", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true) });
	const refuse = async (peer: Stream) => {
		await peer.writer.u53(RequestError.id);
		await new RequestError({ errorCode: 0x10, reasonPhrase: "not here" }).encode(peer.writer, VERSION);
		peer.writer.close();
	};

	const announced = subscriber.announced();
	const namespace = await nextStream(pair.client);
	if (!namespace) throw new Error("no SUBSCRIBE_NAMESPACE was sent");
	expect(await namespace.reader.u53()).toBe(SubscribeNamespace.id);
	await SubscribeNamespace.decode(namespace.reader, VERSION);
	await refuse(namespace);
	expect(await namespace.reader.done()).toBe(true);
	announced.close();

	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	const subscribe = await nextStream(pair.client);
	if (!subscribe) throw new Error("no SUBSCRIBE was sent");
	expect(await subscribe.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(subscribe.reader, VERSION);
	await refuse(subscribe);
	expect(await subscribe.reader.done()).toBe(true);
	track.close();
});

/** One object carrying `properties` as its raw extension bytes, then a one-byte payload. */
function encodeStamped(properties: number[]): Uint8Array {
	// Every field here is under 64, so each is a one-byte varint.
	return new Uint8Array([0, properties.length, ...properties, 1, 42]);
}

/** A group stream header whose objects carry extensions. */
function stampedGroup(groupId: number): GroupMessage {
	return new GroupMessage({
		trackAlias: ALIAS,
		groupId,
		subGroupId: 0,
		publisherPriority: 0,
		flags: { ...groupFlags(true), hasExtensions: true },
	});
}

test("a track without TIMESCALE arrives untimed, even if an object carries a Timestamp", async () => {
	const { subscriber, track } = await subscribeTrack();
	expect((await track.info()).timescale).toBeUndefined();

	// Property 0x10 (Timestamp) = 5, with no units to read it in.
	await subscriber.handleGroup(stampedGroup(0), new Reader(undefined, encodeStamped([0x10, 5]), VERSION));
	const group = await track.ordered().nextGroup();
	const frame = await group?.readFrame();
	expect(frame?.payload).toEqual(new Uint8Array([42]));
	expect(frame?.timestamp).toBeUndefined();
	track.close();
});

test("an object-scope Timescale is never applied", async () => {
	const { subscriber, track } = await subscribeTrack({ properties: { timescale: Timescale.MICRO } });
	expect((await track.info()).timescale).toBe(Timescale.MICRO);

	// Property 0x08 (Timescale) = 1 per second, then 0x10 (Timestamp) = 5 as a type delta of 8.
	await subscriber.handleGroup(stampedGroup(0), new Reader(undefined, encodeStamped([0x08, 1, 0x08, 5]), VERSION));
	const group = await track.ordered().nextGroup();
	const frame = await group?.readFrame();
	expect(frame?.timestamp?.scale).toBe(Timescale.MICRO);
	expect(frame?.timestamp?.value).toBe(5);
	track.close();
});

test("an object without a Timestamp on a TIMESCALE track is malformed", async () => {
	const { subscriber, track } = await subscribeTrack({ properties: { timescale: Timescale.MICRO } });
	const reader = new Reader(undefined, encodeStamped([]), VERSION);
	const stop = spyOn(reader, "stop");

	await subscriber.handleGroup(stampedGroup(0), reader);

	expect(stop).toHaveBeenCalledTimes(1);
	const err = stop.mock.calls[0][0];
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.MalformedTrack);
	// The track can't be trusted past it, so it ends rather than skipping one group.
	expect(track.closed.peek()).toBe(err as StreamError);
	stop.mockRestore();
});

test("a compact END_OF_TRACK at object 5 with properties is subgroup 5, refused without ending the track", async () => {
	for (const version of [Version.DRAFT_18]) {
		const { subscriber, track } = await subscribeTrack({ version });
		const header = stampedGroup(0);
		header.flags.hasSubgroupObject = true;
		// Object ID 5, one unknown integer property, an empty payload, then END_OF_TRACK.
		const reader = new Reader(undefined, new Uint8Array([5, 2, 0x20, 7, 0, 4]), version);
		const stop = spyOn(reader, "stop");

		await subscriber.handleGroup(header, reader);

		expect((stop.mock.calls[0][0] as Error).message).toBe("subgroups are not supported");
		expect(track.closed.peek()).toBeUndefined();
		stop.mockRestore();
	}
});

test("older peer without priority property inherits the default publisher priority", async () => {
	const { subscriber, track } = await subscribeTrack();
	expect((await track.info()).priority).toBe(127 - 128);
	const group = new GroupMessage({
		trackAlias: ALIAS,
		groupId: 3,
		subGroupId: 0,
		publisherPriority: 0,
		flags: { ...groupFlags(true), hasPriority: false },
	});
	await subscriber.handleGroup(group, new Reader(undefined, encodeObjects([0]), VERSION));
	expect(group.publisherPriority).toBe(128);
	track.close();
});

test("an info-only lookup waits for SUBSCRIBE_OK instead of abandoning", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const info = subscriber.consume(Path.from("room")).track("video").info();
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);

	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		trackAlias: ALIAS,
		properties: { priority: 37 },
	}).encode(peer.writer, VERSION);
	expect((await info).priority).toBe(127 - 37);
});

test("SUBSCRIBE_OK's largest is known before any group arrives", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);

	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		trackAlias: ALIAS,
		largest: { groupId: 7n, objectId: 3n },
	}).encode(peer.writer, VERSION);
	// Only the group carries over, since Object IDs are not frame ordinals.
	expect(await track.largest()).toEqual({ group: 7, frame: 0 });
	track.close();
});

test.each(["acceptance", "timeout"])("returning demand preserves pending setup until %s", async (outcome) => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const opening = spyOn(session, "openBi");
	const broadcast = subscriber.consume(Path.from("room"));
	const make = hooks.makeRequest;
	let returned: track.Subscriber | undefined;
	const resumed = Promise.withResolvers<void>();
	const waiting = Promise.withResolvers<void>();
	const unwanted = demandUtil.unwanted;
	const unwantedSpy = spyOn(demandUtil, "unwanted");
	const captured = spyOn(hooks, "makeRequest").mockImplementationOnce((options) => {
		const request = make(options);
		const demand = request.demand();
		let returnedOnce = false;
		unwantedSpy.mockImplementation(async (watched) => {
			if (watched !== demand) return unwanted(watched);
			if (returnedOnce) {
				waiting.resolve();
				return unwanted(demand);
			}
			returnedOnce = true;
			await unwanted(demand);
			const peek = demand.wanted.peek.bind(demand.wanted);
			// Return after the watcher checks demand, before its result crosses the
			// promise race to the setup continuation. No real-time sleeps are involved.
			const observed = spyOn(demand.wanted, "peek").mockImplementationOnce(() => {
				const used = peek();
				observed.mockRestore();
				queueMicrotask(() => {
					returned = broadcast.track("video").subscribe();
					resumed.resolve();
				});
				return used;
			});
		});
		return request;
	});
	const first = broadcast.track("video").subscribe();
	// Restore the ambient clock even if an injection promise outlives the test.
	onTestFinished(() => {
		captured.mockRestore();
		unwantedSpy.mockRestore();
		opening.mockRestore();
		first.close();
		returned?.close();
		broadcast.close();
		session.close();
		installed.close();
	});
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing pending subscribe");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);
	await clock.advance(Nano.fromMilli(Milli(6_000)));
	first.close();
	await resumed.promise;
	if (!returned) throw new Error("demand did not return");
	// Observe the result immediately so a regression is reported as the wrong
	// result, not as an unhandled rejection while the peer writes its response.
	const info = returned.info().then(
		(value) => value,
		(error: unknown) => error,
	);
	expect(await Promise.race([waiting.promise.then(() => true), info.then(() => false)])).toBe(true);
	if (outcome === "acceptance") {
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ trackAlias: ALIAS }).encode(peer.writer, VERSION);
		expect(await info).toMatchObject({ priority: 127 - 128 });
		expect(returned.closed.peek()).toBeUndefined();
	} else {
		await clock.advance(Nano.fromMilli(Milli(4_000)));
		expect(await info).toBeInstanceOf(Error);
		expect(((await info) as Error).message).toContain("request timed out after 10000ms");
	}
	expect(opening).toHaveBeenCalledTimes(1);
});

test("setup abandonment closes before a queued viewer can attach", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const broadcast = subscriber.consume(Path.from("room"));
	const make = hooks.makeRequest;
	const resumed = Promise.withResolvers<track.Subscriber>();
	const unwanted = demandUtil.unwanted;
	const unwantedSpy = spyOn(demandUtil, "unwanted");
	const captured = spyOn(hooks, "makeRequest").mockImplementationOnce((options) => {
		const request = make(options);
		const demand = request.demand();
		unwantedSpy.mockImplementationOnce(async (watched) => {
			await unwanted(watched);
			const peek = demand.wanted.peek.bind(demand.wanted);
			let checks = 0;
			const observed = spyOn(demand.wanted, "peek").mockImplementation(() => {
				const used = peek();
				// The first check is the watcher; the second is the setup loop's
				// final check. A queued viewer must see that cancellation committed.
				if (++checks === 2) {
					observed.mockRestore();
					queueMicrotask(() => resumed.resolve(broadcast.track("video").subscribe()));
				}
				return used;
			});
		});
		return request;
	});
	const first = broadcast.track("video").subscribe();
	let returned: track.Subscriber | undefined;
	// Runner timeouts do not unwind an async test parked on the injection promise.
	onTestFinished(() => {
		captured.mockRestore();
		unwantedSpy.mockRestore();
		first.close();
		returned?.close();
		broadcast.close();
		session.close();
	});
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing initial subscribe");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);
	first.close();
	returned = await resumed.promise;
	const info = returned.info().then(
		(value) => value,
		(error: unknown) => error,
	);
	const next = await Promise.race([nextStream(pair.client), info]);
	expect(next).toBeInstanceOf(Stream);
	if (!(next instanceof Stream)) throw new Error("returning viewer did not get a new subscribe");
	expect(await next.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(next.reader, VERSION);
	await next.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ trackAlias: ALIAS }).encode(next.writer, VERSION);
	expect(await info).toMatchObject({ priority: 127 - 128 });
});

test("early group waits for SUBSCRIBE_OK priority before track acceptance", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);

	const flags = { ...groupFlags(true), hasPriority: false };
	const group = new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags });
	const arriving = subscriber.handleGroup(group, new Reader(undefined, encodeObjects([0]), VERSION));
	const pending = await Promise.race([arriving.then(() => false), Promise.resolve(true)]);
	expect(pending).toBe(true);

	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		trackAlias: ALIAS,
		properties: { priority: 37 },
	}).encode(peer.writer, VERSION);
	await arriving;
	expect((await track.info()).priority).toBe(127 - 37);
	expect(group.publisherPriority).toBe(37);
	const ordered = track.ordered();
	expect((await ordered.nextGroup())?.sequence).toBe(3);
	const explicit = new GroupMessage({
		trackAlias: ALIAS,
		groupId: 4,
		subGroupId: 0,
		publisherPriority: 9,
		flags: groupFlags(true),
	});
	await subscriber.handleGroup(explicit, new Reader(undefined, encodeObjects([0]), VERSION));
	expect(explicit.publisherPriority).toBe(9);
	expect((await track.info()).priority).toBe(127 - 37);
	expect((await ordered.nextGroup())?.sequence).toBe(4);
	ordered.close();
	track.close();
});

/**
 * A group is the unit an application resyncs on, so one served from partway through is
 * unusable: the objects on the stream do not decode without the head the filter excluded,
 * and moq-lite cannot represent the hole at all. Delivering it would pass the group's sixth
 * frame off as the keyframe it opens with, so the group is dropped and the track resumes at
 * the next one. Our own publisher never opens such a stream; a draft-22 peer may.
 */
test("a group served from partway through is dropped", async () => {
	const { subscriber, track } = await subscribeTrack();

	// FIRST_OBJECT clear, and the first object's delta is its absolute id.
	const flags = groupFlags(false);
	const header = new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags });
	await subscriber.handleGroup(header, new Reader(undefined, encodeObjects([5, 0, 0]), VERSION));

	// The next group is served whole, and it is the one the track delivers.
	const whole = groupFlags(true);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 4, subGroupId: 0, publisherPriority: 0, flags: whole }),
		new Reader(undefined, encodeObjects([0, 0]), VERSION),
	);

	const group = await track.ordered().nextGroup();
	expect(group?.sequence).toBe(4);

	track.close();
});

/**
 * The first Object ID is absolute whatever FIRST_OBJECT says, and IDs start at 0, so a
 * draft-18 stream that leaves the bit clear and starts at object 0 is the whole group.
 * The publisher is out of spec on the bit, not missing a head.
 */
test("a clear FIRST_OBJECT at object 0 is the whole group", async () => {
	const { subscriber, track } = await subscribeTrack();

	const flags = groupFlags(false);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags }),
		new Reader(undefined, encodeObjects([0, 0]), VERSION),
	);

	const group = await track.ordered().nextGroup();
	expect(group?.sequence).toBe(3);
	if (!group) return;
	expect(await group.readString()).toBe("object 0");
	expect(group.frameCount).toBe(2);

	track.close();
});

/**
 * The END_OF_GROUP header bit only lets a FIN imply the group's end, so an explicit
 * END_OF_GROUP status on the same stream ends the group there rather than failing it.
 */
test("an END_OF_GROUP status on a marked stream finishes the group", async () => {
	const { subscriber, track } = await subscribeTrack();

	const flags = groupFlags(true);
	expect(flags.hasEnd).toBe(true);
	// Objects 0..4, then object 5 as a zero-length END_OF_GROUP (0x3) status.
	const objects = new Uint8Array([...encodeObjects([0, 0, 0, 0, 0]), 0, 0, 0x3]);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags }),
		new Reader(undefined, objects, VERSION),
	);

	const group = await track.ordered().nextGroup();
	expect(group?.sequence).toBe(3);
	if (!group) return;
	for (let i = 0; i < 5; i++) {
		expect(await group.readString()).toBe("object 0");
	}
	expect(await group.readFrame()).toBeUndefined();
	expect(await group.closed).toBeNull();

	track.close();
});

/**
 * A clear FIRST_OBJECT whose first ID is not 0 still has a hole at the front, so the
 * stream is dropped and the track resumes at the next group.
 */
test("a clear FIRST_OBJECT past object 0 is dropped", async () => {
	const { subscriber, track } = await subscribeTrack();

	const flags = groupFlags(false);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags }),
		new Reader(undefined, encodeObjects([3]), VERSION),
	);

	const whole = groupFlags(true);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 4, subGroupId: 0, publisherPriority: 0, flags: whole }),
		new Reader(undefined, encodeObjects([0]), VERSION),
	);

	const group = await track.ordered().nextGroup();
	expect(group?.sequence).toBe(4);

	track.close();
});

/**
 * A header-only stream with FIRST_OBJECT clear carries none of its group, so it is dropped
 * like any headless stream. With the bit set, the same stream is an
 * empty group. Matches the Rust rule.
 */
test.each([
	[VERSION, false, [4]],
	[VERSION, true, [3, 4]],
])("draft %s with FIRST_OBJECT=%s: a header-only stream delivers %p", async (version, firstObject, expected) => {
	// A budget, so the empty group outlives its successor's arrival.
	const { subscriber, track } = await subscribeTrack({ version, subscription: { maxDelay: Milli(60_000) } });

	await subscriber.handleGroup(
		new GroupMessage({
			trackAlias: ALIAS,
			groupId: 3,
			subGroupId: 0,
			publisherPriority: 0,
			flags: groupFlags(firstObject),
		}),
		new Reader(undefined, new Uint8Array(), version),
	);
	await subscriber.handleGroup(
		new GroupMessage({
			trackAlias: ALIAS,
			groupId: 4,
			subGroupId: 0,
			publisherPriority: 0,
			flags: groupFlags(true),
		}),
		new Reader(undefined, encodeObjects([0]), version),
	);

	const ordered = track.ordered();
	for (const sequence of expected) expect((await ordered.nextGroup())?.sequence).toBe(sequence);
	ordered.close();
	track.close();
});

/**
 * FIRST_OBJECT is the publisher's claim, and the object ids are what actually happened. A
 * peer that sets the bit and then starts at object 5 is contradicting itself, so the group
 * is aborted rather than delivered with a hole the header said was not there.
 */
test("a group that claims its first object must start at zero", async () => {
	const { subscriber, track } = await subscribeTrack();

	const flags = groupFlags(true);
	const header = new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags });
	await subscriber.handleGroup(header, new Reader(undefined, encodeObjects([5, 0, 0]), VERSION));

	const group = await track.ordered().nextGroup();
	expect(group).toBeDefined();
	if (!group) return;
	await expect(group.readFrameSequence()).rejects.toThrow(/object IDs must start at 0/);

	track.close();
});

for (const [name, firstObject, bytes] of [
	["the first object ID", false, []],
	["the first object", true, []],
	["the first payload after peeking object zero", false, [0]],
] as const) {
	test(`unsubscribing stops a subgroup stalled before ${name}`, async () => {
		const { subscriber, track } = await subscribeTrack();
		const opened = Promise.withResolvers<Tail>();
		const open = Tail.prototype.open;
		const opening = spyOn(Tail.prototype, "open").mockImplementation(function (this: Tail, sequence) {
			opened.resolve(this);
			return open.call(this, sequence);
		});
		const cancelled = jest.fn();
		let controller: ReadableStreamDefaultController<Uint8Array>;
		const readable = new ReadableStream<Uint8Array>({
			start(value) {
				controller = value;
			},
			cancel: cancelled,
		});
		const reader = new Reader(readable, new Uint8Array(bytes), VERSION);
		const header = new GroupMessage({
			trackAlias: ALIAS,
			groupId: 3,
			subGroupId: 0,
			publisherPriority: 0,
			flags: groupFlags(firstObject),
		});
		const handled = subscriber.handleGroup(header, reader);
		onTestFinished(async () => {
			track.close();
			if (cancelled.mock.calls.length === 0) controller.close();
			await handled;
			opening.mockRestore();
		});
		const tail = await opened.promise;
		track.close();
		await handled;
		expect(cancelled).toHaveBeenCalledTimes(1);
		// A completed handler must also release the live Tail entry, not just stop its reader.
		await tail.settle(() => true, new Once<null>());
	});
}

test("every object in a chunk reaches the reader before it wakes", async () => {
	const { subscriber, track } = await subscribeTrack();

	const header = new GroupMessage({
		trackAlias: ALIAS,
		groupId: 3,
		subGroupId: 0,
		publisherPriority: 0,
		flags: groupFlags(true),
	});
	const objects = encodeObjects(Array.from({ length: 10 }, () => 0));
	const readable = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(objects);
			controller.close();
		},
	});
	const handled = subscriber.handleGroup(header, new Reader(readable, undefined, VERSION));

	const group = await track.ordered().nextGroup();
	if (!group) throw new Error("no group");
	expect(await group.readString()).toBe("object 0");
	expect(group.frameCount).toBe(10);

	await handled;
	track.close();
});

test("local readers filter hidden unsolicited namespaces from a legacy peer", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true) });
	const plain = subscriber.announced();
	const opted = subscriber.announced(undefined, { hidden: true });
	const handlers: Promise<void>[] = [];
	const streams: Stream[] = [];
	for (const path of [".stats/node", "visible"]) {
		const stream = await Stream.open(pair.server, { version: VERSION });
		streams.push(stream);
		handlers.push(
			subscriber.runPublishNamespace(
				new PublishNamespace({ requestId: 0n, trackNamespace: Path.from(path) }),
				stream,
			),
		);
	}
	expect((await plain.next())?.prefix).toBe(Path.from("visible"));
	expect((await opted.next())?.prefix).toBe(Path.from(".stats/node"));
	expect((await opted.next())?.prefix).toBe(Path.from("visible"));
	for (const stream of streams) stream.close();
	plain.close();
	opted.close();
	await Promise.all(handlers);
});

test("object extension limit accepts 64 KiB and stops one byte over before reading", async () => {
	for (const size of [65536, 65537]) {
		for (const first of [true, false]) {
			const { subscriber, track } = await subscribeTrack();
			const bytes = new Uint8Array((first ? 0 : 4) + 4 + (size === 65536 ? size + 2 : 0));
			let offset = 0;
			if (!first) {
				bytes.set([0, 0, 1, 42]);
				offset = 4;
			}
			// Object delta zero followed by a three-byte leading-ones varint length.
			bytes.set([0, 0xc1, 0, size - 65536], offset);
			if (size === 65536) bytes.set([1, 42], bytes.length - 2);
			const reader = new Reader(undefined, bytes, VERSION);
			const stop = spyOn(reader, "stop");
			const header = new GroupMessage({
				trackAlias: ALIAS,
				groupId: 3,
				subGroupId: 0,
				publisherPriority: 0,
				flags: { ...groupFlags(true), hasExtensions: true },
			});
			await subscriber.handleGroup(header, reader);
			if (size === 65536) {
				expect(stop.mock.calls).toEqual([]);
				const group = await track.ordered().nextGroup();
				expect(group?.frameCount).toBe(first ? 1 : 2);
				expect(await group?.readString()).toBe("*");
			} else {
				expect(stop).toHaveBeenCalledTimes(1);
				const err = stop.mock.calls[0][0];
				expect(err).toBeInstanceOf(StreamError);
				expect((err as StreamError).code).toBe(StreamCode.MalformedTrack);
			}
			stop.mockRestore();
			track.close();
		}
	}
});

/**
 * An OBJECT_DATAGRAM at object 0 is a datagram at its Group ID; anything the model cannot
 * carry is dropped like a lost datagram, and a malformed one ends the session.
 */
test("an object datagram is a datagram group", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), quic: pair.server });
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("the subscriber never opened a subscribe stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		trackAlias: ALIAS,
		properties: { timescale: Timescale.MILLI },
	}).encode(peer.writer, VERSION);
	await track.info();

	const receiving = subscriber.runDatagrams();
	const writer = pair.client.datagrams.writable.getWriter();
	const send = (fields: Partial<ConstructorParameters<typeof ObjectDatagram>[0]>) =>
		writer.write(
			new ObjectDatagram({
				trackAlias: ALIAS,
				groupId: 4,
				endOfGroup: true,
				body: { payload: new TextEncoder().encode("no") },
				...fields,
			}).encode(VERSION),
		);

	// Past object 0, an unbound alias, a status other than Normal, and no Timestamp on a timed track.
	await send({ objectId: 1 });
	await send({ trackAlias: ALIAS + 1n });
	await send({ endOfGroup: false, body: { status: 3 } });
	await send({ groupId: 5, objectId: 0 });
	const properties = encodeObjectExtensions(Timestamp.fromMillis(1234), Timescale.MILLI, VERSION);
	await send({ groupId: 9, objectId: 0, publisherPriority: 7, properties, body: { payload: Uint8Array.of(1) } });

	const datagram = await track.recvDatagram();
	expect(datagram?.sequence).toBe(9);
	expect(datagram?.payload).toEqual(Uint8Array.of(1));
	expect(datagram?.timestamp?.as(Timescale.MILLI)).toBe(1234);

	// A status cannot end the group.
	await writer.write(Uint8Array.of(0x22, Number(ALIAS), 4, 0, 0));
	expect(
		await receiving.then(
			() => undefined,
			(err: unknown) => err,
		),
	).toBeInstanceOf(ProtocolViolation);
	track.close();
});

test("a more specific epochless announcement replaces the consumed source", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true) });
	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);
	const pool = Path.from("pool");
	const job = Path.from("pool/job");
	const broadStream = await Stream.open(pair.server, { version: VERSION });
	const broad = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: pool }),
		broadStream,
	);
	await announced.next();
	const broadPeer = await nextStream(pair.client);
	if (!broadPeer) throw new Error("no broad publish stream");
	await broadPeer.reader.u53();
	await RequestOk.decode(broadPeer.reader, VERSION);
	const held = subscriber.consume(job);
	const specificStream = await Stream.open(pair.server, { version: VERSION });
	const specific = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 2n, trackNamespace: job }),
		specificStream,
	);
	await announced.next();
	const specificPeer = await nextStream(pair.client);
	if (!specificPeer) throw new Error("no specific publish stream");
	await specificPeer.reader.u53();
	await RequestOk.decode(specificPeer.reader, VERSION);
	const fresh = subscriber.consume(job);
	expect(fresh.closed).not.toBe(held.closed);
	expect(held.closed.peek()).toBeUndefined();
	specificPeer.close();
	await specific;
	await announced.next();
	const fallback = subscriber.consume(job);
	expect(fallback.closed).toBe(held.closed);
	broadPeer.close();
	await broad;
	for (const consumer of [held, fresh, fallback]) consumer.close();
	announced.close();
});

for (const version of [Version.DRAFT_18, Version.DRAFT_22]) {
	test(`status zero without the end bit delivers an empty object and continues (${version})`, async () => {
		const { subscriber, track } = await subscribeTrack({ version });
		onTestFinished(() => track.close());
		const message = new GroupMessage({
			trackAlias: ALIAS,
			groupId: 0,
			subGroupId: 0,
			publisherPriority: 0,
			flags: { ...groupFlags(true), hasEnd: false },
		});
		// Empty normal object, a nonempty successor, then explicit group end.
		await subscriber.handleGroup(
			message,
			new Reader(undefined, new Uint8Array([0, 0, 0, 0, 1, 42, 0, 0, 3]), version),
		);
		const group = await track.ordered().nextGroup();
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array());
		expect((await group?.readFrame())?.payload).toEqual(new Uint8Array([42]));
		expect(await group?.readFrame()).toBeUndefined();
	});
}

/**
 * A request on another stream can overtake a cancel, so a peer that refuses overlapping
 * requests (PREFIX_OVERLAP) would see the broader one beside the narrower one it replaces.
 * It goes out only once the peer has ended the narrower one's stream.
 */
test("a request overlapping one on its way out waits for the peer to end it", async () => {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });

	const narrow = subscriber.announced(Path.Pattern.subtree(Path.from("a/b")));
	const stream = await acceptSubscribeNamespace(pair.client, version);
	await inlineNamespace(stream, Path.from("x"), undefined, version);
	expect(await narrow.next()).toMatchObject({ prefix: Path.from("a/b/x"), kind: "start" });

	// A request still open is no blocker, and hears the narrower one's route go at once.
	const watcher = subscriber.announced(Path.Pattern.subtree(Path.from("a/b")));
	const watching = await acceptSubscribeNamespace(pair.client, version);
	expect(await watcher.next()).toMatchObject({ prefix: Path.from("a/b/x"), kind: "start" });
	narrow.close();
	const broad = subscriber.announced(Path.Pattern.subtree(Path.from("a")));
	expect(await watcher.next()).toMatchObject({ prefix: Path.from("a/b/x"), kind: "end" });

	// Draft 22 cancels with a reset, and the broader request waits on the peer's half.
	expect(stream.reader.done()).rejects.toMatchObject({ code: StreamCode.Cancel });
	const incoming = pair.client.incomingBidirectionalStreams.getReader();
	let sent = false;
	const opened = incoming.read().then((next) => {
		sent = true;
		return next;
	});

	// The closed reader is still registered meanwhile, and hears nothing more.
	await inlineNamespace(watching, Path.from("y"), undefined, version);
	expect(await watcher.next()).toMatchObject({ prefix: Path.from("a/b/y"), kind: "start" });
	expect(sent).toBe(false);

	stream.writer.close();
	const next = await opened;
	incoming.releaseLock();
	if (next.done) throw new Error("the broader SUBSCRIBE_NAMESPACE never went out");
	const asked = new Stream({ readable: next.value.readable, writable: next.value.writable, version });
	await asked.reader.u53();
	expect((await SubscribeNamespace.decode(asked.reader, version)).namespace).toBe(Path.from("a"));

	watcher.close();
	broad.close();
	session.close();
});

/** Answer the next SUBSCRIBE_NAMESPACE with REQUEST_OK, carrying `activeCount` when given. */
async function answerCounted(transport: WebTransport, activeCount: number | undefined): Promise<Stream> {
	const stream = await nextStream(transport);
	if (!stream) throw new Error("no SUBSCRIBE_NAMESPACE was sent");
	await stream.reader.u53();
	await SubscribeNamespace.decode(stream.reader, VERSION);
	const parameters = new Parameters();
	parameters.activeCount = activeCount;
	await stream.writer.u53(RequestOk.id);
	await new RequestOk({ parameters }).encode(stream.writer, VERSION);
	return stream;
}

const sessionClosed = (pair: ReturnType<typeof createMockTransportPair>) =>
	Promise.race([
		pair.server.closed,
		new Promise<never>((_resolve, reject) => setTimeout(() => reject(new Error("session stayed up")), STREAM_WAIT)),
	]);

/** The session closes with PROTOCOL_VIOLATION, not a clean code. */
async function violationClosed(pair: ReturnType<typeof createMockTransportPair>) {
	expect((await sessionClosed(pair)).closeCode).toBe(new ProtocolViolation("").code);
}

const yieldTask = () => new Promise((resolve) => setTimeout(resolve, 0));

/** MoQ Active Count: the consumer syncs once the counted NAMESPACEs are read. */
test("counted NAMESPACEs sync", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, activeCount: true });

	const announced = subscriber.announced();
	const subscription = await answerCounted(pair.client, 2);
	await inlineNamespace(subscription, Path.from("a"));
	expect(await announced.next()).toMatchObject({ prefix: Path.from("a"), kind: "start" });
	const b = announced.next();
	await yieldTask();
	expect(announced.synced.peek()).toBe(false);

	await inlineNamespace(subscription, Path.from("b"));
	expect(await b).toMatchObject({ prefix: Path.from("b"), kind: "start" });
	await yieldTask();
	// Still handling the last of them.
	expect(announced.synced.peek()).toBe(false);
	void announced.next();
	await yieldTask();
	expect(announced.synced.peek()).toBe(true);
	announced.close();
});

/** Without MoQ Active Count, REQUEST_OK is the best boundary there is. */
test("an uncounted answer syncs on the OK", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();
	await yieldTask();
	expect(announced.synced.peek()).toBe(false);
	await acceptSubscribeNamespace(pair.client);
	await yieldTask();
	expect(announced.synced.peek()).toBe(true);
	announced.close();
});

/** A count the negotiation did not promise, or a missing one it did, breaks the extension. */
test("an ACTIVE_COUNT that does not match the negotiation closes the session", async () => {
	for (const [negotiated, count] of [
		[true, undefined],
		[false, 0],
	] as const) {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const session = new NativeSession(pair.server, VERSION, true);
		const subscriber = new Subscriber({ session, quic: pair.server, activeCount: negotiated });
		const warn = spyOn(console, "warn").mockImplementation(() => void 0);
		onTestFinished(() => warn.mockRestore());

		subscriber.announced();
		await answerCounted(pair.client, count);
		await violationClosed(pair);
	}
});

/** A violation on a namespace subscription closes the session through its request, once. */
test("a namespace subscription closes the session once over a violation", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, quic: pair.server, activeCount: true });
	const warn = spyOn(console, "warn").mockImplementation(() => void 0);
	onTestFinished(() => warn.mockRestore());
	const close = spyOn(pair.server, "close");

	subscriber.announced();
	await answerCounted(pair.client, undefined);
	await violationClosed(pair);
	await yieldTask();
	expect(close.mock.calls.filter(([info]) => info?.closeCode !== undefined)).toHaveLength(1);
});

/** A counted set cut short, by a NAMESPACE_DONE or the stream ending, is the peer's fault. */
test.each(["done", "fin"] as const)("a counted set cut short by %s closes the session", async (cut) => {
	{
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const session = new NativeSession(pair.server, VERSION, true);
		const subscriber = new Subscriber({ session, quic: pair.server, activeCount: true });
		const warn = spyOn(console, "warn").mockImplementation(() => void 0);
		onTestFinished(() => warn.mockRestore());

		subscriber.announced();
		const subscription = await answerCounted(pair.client, 2);
		await inlineNamespace(subscription, Path.from("a"));
		if (cut === "done") {
			// The type alone is the violation, so the session may close before the rest is written.
			await subscription.writer.u53(SubscribeNamespaceEntryDone.id);
			await new SubscribeNamespaceEntryDone({ suffix: Path.from("a") })
				.encode(subscription.writer, VERSION)
				.catch(() => undefined);
		} else {
			subscription.writer.close();
		}
		await violationClosed(pair);
	}
});

for (const negotiated of [false, true]) {
	test(`subscriber max age is sent only to a declaring peer: ${negotiated}`, async () => {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const subscriber = new Subscriber({
			session: new NativeSession(pair.server, VERSION, true),
			maxAge: negotiated,
		});
		const track = subscriber
			.consume(Path.from("room"))
			.track("video")
			.subscribe({ maxDelay: Milli(250) });
		const peer = await nextStream(pair.client);
		if (!peer) throw new Error("no subscribe stream");
		expect(await peer.reader.u53()).toBe(Subscribe.id);
		const request = await Subscribe.decode(peer.reader, VERSION);
		expect(request.maxAge).toBe(negotiated ? 250 : undefined);
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ trackAlias: ALIAS }).encode(peer.writer, VERSION);
		await track.info();
		// The aggregate can change before SUBSCRIBE_OK: the first update must catch it.
		const tolerant = track.fork({ maxDelay: Milli(900), priority: 9 });
		expect(await peer.reader.u53()).toBe(SubscribeUpdate.id);
		const update = await SubscribeUpdate.decode(peer.reader, VERSION);
		expect(update.parameters.maxAge).toBe(negotiated ? 900 : undefined);
		expect(update.parameters.subscriberPriority).toBe(127 - 9);
		tolerant.close();
		track.close();
		peer.close();
	});
}
/** Advertise `path` inline under `epoch`, on a session that negotiated MoQ Epoch alone. */
async function inlineEpoch(stream: Stream, path: Path.Valid, epoch: Epoch.Valid): Promise<void> {
	await stream.writer.u53(SubscribeNamespaceEntry.id);
	await new SubscribeNamespaceEntry({ suffix: path, params: { epoch } }).encode(stream.writer, VERSION);
}

/** Open a PUBLISH_NAMESPACE for `path` under `epoch`, returning the handler and the peer's end. */
async function publishEpoch(
	pair: ReturnType<typeof createMockTransportPair>,
	subscriber: Subscriber,
	path: Path.Valid,
	epoch: Epoch.Valid,
): Promise<{ handler: Promise<void>; peer: Stream }> {
	const stream = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: path, params: { epoch } }),
		stream,
	);
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(peer.reader, VERSION);
	return { handler, peer };
}

/** The route names the advertised instance, and so does every SUBSCRIBE sent for it. */
test("an advertised epoch reaches the route and the SUBSCRIBE", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), epoch: true });
	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const epoch = Epoch.mint();
	await publishEpoch(pair, subscriber, Path.from("room"), epoch);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start", route: { epoch } });

	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	onTestFinished(() => track.close());
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	expect((await Subscribe.decode(peer.reader, VERSION)).epoch).toBe(epoch);
	announced.close();
});

test("an inline NAMESPACE route carries its epoch", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), epoch: true });
	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const epoch = Epoch.mint();
	await inlineEpoch(subscription, Path.from("room"), epoch);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start", route: { epoch } });
	announced.close();
});

/**
 * A NAMESPACE repeated under another epoch replaces the instance in place: the path
 * restarts and is never absent, and a fresh reader hears the new instance standing.
 */
test("a NAMESPACE repeated under another epoch restarts the path in place", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), epoch: true });
	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const path = Path.from("room");
	const first = Epoch.mint();
	await inlineEpoch(subscription, path, first);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: { epoch: first } });

	const second = Epoch.mint();
	await inlineEpoch(subscription, path, second);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart", route: { epoch: second } });

	const fresh = subscriber.announced();
	expect(await fresh.next()).toMatchObject({ prefix: path, kind: "start", route: { epoch: second } });
	fresh.close();
	announced.close();
});

/**
 * A namespace advertised both inline and by a later PUBLISH_NAMESPACE takes another epoch
 * from either: an inline repeat under the new epoch is the newest advertisement, not an
 * update to the older of the two.
 */
test("a NAMESPACE repeat under another epoch outranks a later PUBLISH_NAMESPACE", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), epoch: true });
	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const path = Path.from("room");
	const first = Epoch.mint();
	await inlineEpoch(subscription, path, first);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: { epoch: first } });
	await publishEpoch(pair, subscriber, path, first);

	const second = Epoch.mint();
	await inlineEpoch(subscription, path, second);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart", route: { epoch: second } });
	announced.close();
});

/**
 * A NAMESPACE that loops back through us detaches its route, but the advertisement stays
 * live on the wire until its NAMESPACE_DONE: a repeat that restores a clean path while
 * losing its epoch is still the violation it would be without the loop.
 */
test("a reflected NAMESPACE keeps its epoch live", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, quic: pair.server, cluster: { self: SELF, peer: PEER }, epoch: true });
	const warn = spyOn(console, "warn").mockImplementation(() => void 0);
	onTestFinished(() => warn.mockRestore());
	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const epoch = Epoch.mint();
	const send = async (hops: Hop[], named?: Epoch.Valid) => {
		await subscription.writer.u53(SubscribeNamespaceEntry.id);
		await new SubscribeNamespaceEntry({
			suffix: Path.from("room"),
			params: { cluster: { hops, cost: 0n }, epoch: named },
		}).encode(subscription.writer, VERSION);
	};
	await send([PEER], epoch);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	await send([SELF, PEER], epoch);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "end" });

	await send([PEER]);
	await violationClosed(pair);
});

/**
 * Gaining or losing an epoch is not one instance replacing another, so a repeat that does
 * either means the peer skipped the NAMESPACE_DONE.
 */
test.each(["gains", "loses"] as const)("a NAMESPACE repeat that %s an epoch closes the session", async (change) => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, quic: pair.server, epoch: true });
	const warn = spyOn(console, "warn").mockImplementation(() => void 0);
	onTestFinished(() => warn.mockRestore());
	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const epoch = Epoch.mint();
	const send = async (path: Path.Valid, named?: Epoch.Valid) => {
		await subscription.writer.u53(SubscribeNamespaceEntry.id);
		await new SubscribeNamespaceEntry({ suffix: path, params: { epoch: named } }).encode(
			subscription.writer,
			VERSION,
		);
	};
	const [before, after] = change === "gains" ? [undefined, epoch] : [epoch, undefined];
	await send(Path.from("room"), before);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });

	// Control: a repeat that keeps the epoch, or its absence, is not a violation.
	await send(Path.from("room"), before);
	await send(Path.from("sentinel"), before);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("sentinel"), kind: "start" });

	await send(Path.from("room"), after);
	await violationClosed(pair);
});

/**
 * Two PUBLISH_NAMESPACE requests travel on independent streams, so a restarted publisher's
 * new advertisement can land before its old one's withdrawal. The newest stands throughout.
 */
test("a stale instance's withdrawal arriving last keeps the newer epoch", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const subscriber = new Subscriber({ session: new NativeSession(pair.server, VERSION, true), epoch: true });
	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const path = Path.from("room");
	const first = Epoch.mint();
	const old = await publishEpoch(pair, subscriber, path, first);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "start", route: { epoch: first } });

	const second = Epoch.mint();
	await publishEpoch(pair, subscriber, path, second);
	expect(await announced.next()).toMatchObject({ prefix: path, kind: "restart", route: { epoch: second } });

	old.peer.close();
	await old.handler;

	const next = await Promise.race([
		announced.next(),
		new Promise<"nothing">((resolve) => setTimeout(() => resolve("nothing"), 250)),
	]);
	expect(next).toBe("nothing");

	// A fresh reader hears the newer instance as the one standing.
	const fresh = subscriber.announced();
	expect(await fresh.next()).toMatchObject({ prefix: path, kind: "start", route: { epoch: second } });
	fresh.close();
	announced.close();
});
