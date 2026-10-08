import { expect, jest, onTestFinished, spyOn, test } from "bun:test";
import type * as announce from "../announced.ts";
import { ProtocolViolation, StreamCode, Stream as StreamError } from "../error.ts";
import { type Hop, HopSchema, UNKNOWN_HOP } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Timescale } from "../time.ts";
import type * as track from "../track.ts";
import { ControlStreamAdapter, NativeSession } from "./adapter.ts";
import type * as Cluster from "./cluster.ts";
import { Connection } from "./connection.ts";
import { type GroupFlags, Group as GroupMessage } from "./object.ts";
import type { Properties } from "./properties.ts";
import { PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Subscribe, SubscribeOk, Unsubscribe } from "./subscribe.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** How long to wait for a stream before calling it absent. */
const STREAM_WAIT = 500;

/**
 * Accept the next stream the subscriber opens, or give up rather than hang forever.
 *
 * Reads the queue directly instead of racing {@link Stream.accept}, whose pending read
 * would keep the reader locked after the race resolves and could swallow a later stream.
 */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
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
		return new Stream({ readable: next.value.readable, writable: next.value.writable, version: VERSION });
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);

	const subscriber = new Subscriber({ session });
	subscriber.announced();

	expect(await nextStream(pair.client)).toBeDefined();
});

/**
 * Draft-16 is the first that allows a zero-field track namespace. Asking a foreign peer
 * (one that declared no MoQ Solicit) for one earlier is a protocol violation, so an
 * unscoped subscriber sends nothing and still takes an unsolicited announcement.
 */
test.each([
	["draft-14", Version.DRAFT_14, ALPN.DRAFT_14],
	["draft-15", Version.DRAFT_15, ALPN.DRAFT_15],
] as const)("%s does not ask a foreign peer for the empty namespace", async (_name, version, alpn) => {
	const pair = createMockTransportPair(alpn);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });
	const announced = subscriber.announced();

	expect(await nextStream(pair.client)).toBeUndefined();

	const stream = await Stream.open(pair.server, { version });
	void subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("surprise") }),
		stream,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("surprise"), kind: "start" });
	announced.close();
});

/** With no request stream to end it, the unasked reader ends with the session, from either side. */
test.each([
	["local", "server"],
	["remote", "client"],
] as const)("an unasked draft-14 reader ends on a %s close", async (_name, side) => {
	const pair = createMockTransportPair(ALPN.DRAFT_14);
	const session = new NativeSession(pair.server, Version.DRAFT_14, true);
	const subscriber = new Subscriber({ session, quic: pair.server });
	const announced = subscriber.announced();

	pair[side].close();

	expect(await announced.next()).toBeUndefined();
});

/** A peer that declared MoQ Solicit is ours: it only tells when asked, so it still is. */
test.each([
	["draft-14", Version.DRAFT_14, ALPN.DRAFT_14],
	["draft-15", Version.DRAFT_15, ALPN.DRAFT_15],
] as const)("%s still asks a soliciting peer for the empty namespace", async (_name, version, alpn) => {
	const pair = createMockTransportPair(alpn);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session, solicit: true });

	subscriber.announced();

	expect(await nextStream(pair.client)).toBeDefined();
});

/** A named prefix is still legal on the drafts that reject the empty one. */
test("draft-14 still asks for a named prefix", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_14);
	const session = new NativeSession(pair.server, Version.DRAFT_14, true);
	const subscriber = new Subscriber({ session });

	subscriber.announced(Path.Pattern.subtree(Path.from("cam")));

	expect(await nextStream(pair.client)).toBeDefined();
});

/** The empty prefix is the "every namespace" request from draft-16 on. */
test("draft-16 still asks for the empty namespace", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_16);
	const session = new NativeSession(pair.server, Version.DRAFT_16, true);
	const subscriber = new Subscriber({ session });

	subscriber.announced();

	expect(await nextStream(pair.client)).toBeDefined();
});

/**
 * The other half of discovery: a peer that tells us unasked. Asking must not make us deaf
 * to a PUBLISH_NAMESPACE that arrives on its own stream instead.
 */
test("an unsolicited announcement lands", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();

	// The question we asked, which this peer never answers.
	expect(await nextStream(pair.client)).toBeDefined();

	// What the connection dispatch does when a PUBLISH_NAMESPACE arrives instead.
	const stream = await Stream.open(pair.server, { version: VERSION });
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
	const second = await Stream.open(pair.server, { version: VERSION });
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
async function acceptSubscribeNamespace(transport: WebTransport): Promise<Stream> {
	const stream = await nextStream(transport);
	if (!stream) throw new Error("no SUBSCRIBE_NAMESPACE was sent");

	// Drain the request, then answer it.
	await stream.reader.u53();
	await SubscribeNamespace.decode(stream.reader, VERSION);
	await stream.writer.u53(RequestOk.id);
	await new RequestOk({ requestId: undefined }).encode(stream.writer, VERSION);

	return stream;
}

/** Advertise `path` inline on a SUBSCRIBE_NAMESPACE stream. */
async function inlineNamespace(stream: Stream, path: Path.Valid, cluster?: Cluster.Advert): Promise<void> {
	await stream.writer.u53(SubscribeNamespaceEntry.id);
	await new SubscribeNamespaceEntry({ suffix: path, cluster }).encode(stream.writer, VERSION);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
 * Draft-14/15 name their namespace-scoped messages instead of numbering them, so the
 * adapter can hold one request per namespace. A second would overwrite the first and the
 * withdrawals would go to the wrong stream, then to none at all, killing the session. The
 * case that makes a second reference legitimate needs an inline NAMESPACE, which those
 * drafts do not have.
 */
test("a duplicate legacy publish_namespace is still refused", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, Version.DRAFT_15, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();

	const first = await Stream.open(pair.server, { version: Version.DRAFT_15 });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("twice") }),
		first,
	);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("twice"), kind: "start" });

	// The same namespace again, on its own request.
	const second = await Stream.open(pair.server, { version: Version.DRAFT_15 });
	await subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 2n, trackNamespace: Path.from("twice") }),
		second,
	);

	// Refused, so it took no reference: the first request ending still retracts the path.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await handler;

	expect(await announced.next()).toMatchObject({ prefix: Path.from("twice"), kind: "end" });
});

/**
 * The read loop is not awaited when the local consumer closes first, and closing the
 * stream cancels the transport without discarding what the reader already buffered. An
 * entry that decodes after the teardown must not take a reference, or the path is pinned
 * for the session with nobody left to release it.
 */
test("an entry buffered past a consumer close does not pin the path", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Both entries land in one chunk, so the second is buffered while the first is being
	// delivered. Closing the consumer then races the loop that would attach it.
	await inlineNamespace(subscription, Path.from("first"));
	await inlineNamespace(subscription, Path.from("buffered"));

	announced.close();

	// Whatever the loop managed to attach, the stream gave back on its way out.
	await new Promise((resolve) => setTimeout(resolve, 50));

	const fresh = subscriber.announced();
	const seeded = await Promise.race([
		fresh.next(),
		new Promise<"nothing">((resolve) => setTimeout(() => resolve("nothing"), 250)),
	]);
	expect(seeded).toBe("nothing");
});

/**
 * The reservation has to be synchronous. The count is only taken once the OK is written,
 * so two legacy requests dispatched together would both get past a check that looked only
 * at what is announced, and both would take a reference for one namespace.
 */
test("concurrent legacy publish_namespace requests take one reference", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, Version.DRAFT_15, true);
	const subscriber = new Subscriber({ session });

	const announced = subscriber.announced();

	// Dispatched together, as the connection would on two incoming streams.
	const first = await Stream.open(pair.server, { version: Version.DRAFT_15 });
	const second = await Stream.open(pair.server, { version: Version.DRAFT_15 });
	const one = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("raced") }),
		first,
	);
	const two = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 2n, trackNamespace: Path.from("raced") }),
		second,
	);

	expect(await announced.next()).toMatchObject({ prefix: Path.from("raced"), kind: "start" });
	await two;

	// Only one reference was taken, so the surviving request ending retracts the path.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await one;

	expect(await announced.next()).toMatchObject({ prefix: Path.from("raced"), kind: "end" });
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	const subscription = await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	await subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("mine"),
			cluster: { hops: [SELF, PEER], cost: 0n },
		}),
		request,
	);

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	expect(await peer.reader.u53()).toBe(RequestError.id);
	const err = await RequestError.decode(peer.reader, VERSION);
	// UNINTERESTED, draft-19 section 15.11.2: stop offering us this namespace.
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const control = await Stream.open(pair.server, { version: VERSION });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		maxRequestId: 100n,
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
			cluster: { hops: [PEER], cost: 0n },
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
			cluster: { hops: [publisher, PEER], cost: 0n },
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			cluster: { hops: [PEER], cost: 4n },
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({
			requestId: 0n,
			trackNamespace: Path.from("theirs"),
			cluster: { hops: [HopSchema.parse(11n), PEER], cost: 0n },
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

/**
 * A second PUBLISH_NAMESPACE on the stream that already carries one is no longer an
 * update: it is the base draft's duplicate request, a protocol violation.
 */
test("a repeated PUBLISH_NAMESPACE is a protocol violation", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session, cluster: { self: SELF, peer: PEER } });

	const announced = subscriber.announced();
	await acceptSubscribeNamespace(pair.client);

	const advert = new PublishNamespace({
		requestId: 0n,
		trackNamespace: Path.from("theirs"),
		cluster: { hops: [PEER], cost: 0n },
	});
	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(advert, request);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "start" });

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream");
	await peer.writer.u53(PublishNamespace.id);
	await advert.encode(peer.writer, VERSION);

	await expect(handler).rejects.toThrow(ProtocolViolation);
	expect(await announced.next()).toMatchObject({ prefix: Path.from("theirs"), kind: "end" });
});

/**
 * Drafts 14-16 carry every request over the control stream adapter's virtual streams,
 * whose abort is local. UNSUBSCRIBE is the only cancellation that reaches the peer there,
 * so a cancelled subscription is only really cancelled if that message lands on the real
 * control stream.
 */
test("a legacy cancel reaches the control stream", async () => {
	const LEGACY = Version.DRAFT_16;
	const pair = createMockTransportPair(ALPN.DRAFT_16);

	// The one real bidi everything is multiplexed onto.
	const controlStream = await Stream.open(pair.server, { version: LEGACY });
	const session = new ControlStreamAdapter(pair.server, controlStream, LEGACY, 100n, true);
	const subscriber = new Subscriber({ session });

	// The peer's view of that control stream.
	const peer = await nextStream(pair.client);
	expect(peer).toBeDefined();

	// Ask for a track, which writes SUBSCRIBE, then drop the only consumer.
	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();

	// Let the SUBSCRIBE reach the control stream before walking away.
	await new Promise((resolve) => setTimeout(resolve, 50));
	track.close();

	// Everything the subscriber actually put on the wire.
	const seen: bigint[] = [];
	const deadline = Date.now() + STREAM_WAIT;
	while (Date.now() < deadline) {
		const type = await Promise.race([
			// biome-ignore lint/style/noNonNullAssertion: guarded by the expect above
			peer!.reader.u53().catch(() => undefined),
			new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), 100)),
		]);
		if (type === undefined) break;

		seen.push(BigInt(type));
		// biome-ignore lint/style/noNonNullAssertion: guarded by the expect above
		const size = await peer!.reader.u16();
		// biome-ignore lint/style/noNonNullAssertion: guarded by the expect above
		if (size > 0) await peer!.reader.read(size);
		if (BigInt(type) === BigInt(Unsubscribe.id)) break;
	}

	expect(seen).toContain(BigInt(Unsubscribe.id));
});

/**
 * A publisher that rejects a SUBSCRIBE has torn the request down before answering, so there
 * is nothing left to cancel. Naming a dead request id back at it is what a strict peer can
 * read as a protocol violation and close an otherwise healthy session over.
 */
test("a rejected subscribe is not unsubscribed", async () => {
	const LEGACY = Version.DRAFT_16;
	const pair = createMockTransportPair(ALPN.DRAFT_16);

	const controlStream = await Stream.open(pair.server, { version: LEGACY });
	const session = new ControlStreamAdapter(pair.server, controlStream, LEGACY, 100n, true);
	// The mux read loop, which is what routes a response back to its virtual stream.
	void session.run().catch(() => void 0);
	const subscriber = new Subscriber({ session });

	const peer = await nextStream(pair.client);
	expect(peer).toBeDefined();
	// biome-ignore lint/style/noNonNullAssertion: guarded above
	const wire = peer!;

	const broadcast = subscriber.consume(Path.from("room"));
	const track = broadcast.track("video").subscribe();

	// Read the SUBSCRIBE, then reject it the way a publisher that cannot serve it would.
	const subscribeType = await wire.reader.u53();
	const subscribeSize = await wire.reader.u16();
	await wire.reader.read(subscribeSize);
	expect(subscribeType).toBe(3);

	await wire.writer.u53(RequestError.id);
	await new RequestError({
		requestId: 0n,
		// DOES_NOT_EXIST, draft-16 section 13.4.2.
		errorCode: 0x10,
		reasonPhrase: "not found",
		retryInterval: 0n,
	}).encode(wire.writer, LEGACY);

	await new Promise((resolve) => setTimeout(resolve, 100));
	track.close();
	await new Promise((resolve) => setTimeout(resolve, 100));

	// Nothing further belongs on the control stream: the request is already gone.
	const next = await Promise.race([
		wire.reader.u53().catch(() => undefined),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), 200)),
	]);

	expect(next).toBeUndefined();
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
}: {
	version?: IetfVersion;
	properties?: Properties;
} = {}): Promise<{ subscriber: Subscriber; track: track.Subscriber }> {
	const pair = createMockTransportPair(version === Version.DRAFT_16 ? ALPN.DRAFT_16 : ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });

	const track = subscriber.consume(Path.from("room")).track("video").subscribe();

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("the subscriber never opened a subscribe stream");
	peer.reader.version = version;
	peer.writer.version = version;

	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, version);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS, properties }).encode(peer.writer, version);

	return { subscriber, track };
}

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

test("older peer without priority property inherits wire priority 128", async () => {
	const { subscriber, track } = await subscribeTrack();
	expect((await track.info()).priority).toBe(0xff - 128);
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
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const info = subscriber.consume(Path.from("room")).track("video").info();
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, VERSION);

	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		requestId: request.requestId,
		trackAlias: ALIAS,
		properties: { priority: 37 },
	}).encode(peer.writer, VERSION);
	expect((await info).priority).toBe(0xff - 37);
});

test.each(["acceptance", "timeout"])("returning demand preserves pending setup until %s", async (outcome) => {
	jest.useFakeTimers();
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const opening = spyOn(session, "openBi");
	const broadcast = subscriber.consume(Path.from("room"));
	const pending = hooks.pendingTrackProducer;
	let returned: track.Subscriber | undefined;
	const resumed = Promise.withResolvers<void>();
	const waiting = Promise.withResolvers<void>();
	const captured = spyOn(hooks, "pendingTrackProducer").mockImplementationOnce((request) => {
		const producer = pending(request);
		const demand = producer.demand();
		const unused = demand.unused.bind(demand);
		let returnedOnce = false;
		spyOn(demand, "unused").mockImplementation(async () => {
			if (returnedOnce) {
				waiting.resolve();
				return unused();
			}
			returnedOnce = true;
			await unused();
			const peek = demand.used.peek.bind(demand.used);
			// Return after the watcher checks demand, before its result crosses the
			// promise race to the setup continuation. No real-time sleeps are involved.
			const observed = spyOn(demand.used, "peek").mockImplementationOnce(() => {
				const used = peek();
				observed.mockRestore();
				queueMicrotask(() => {
					returned = broadcast.track("video").subscribe();
					resumed.resolve();
				});
				return used;
			});
		});
		return producer;
	});
	const first = broadcast.track("video").subscribe();
	// Restore real timers even if an injection promise outlives the test.
	onTestFinished(() => {
		captured.mockRestore();
		opening.mockRestore();
		first.close();
		returned?.close();
		broadcast.close();
		session.close();
		jest.useRealTimers();
	});
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing pending subscribe");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, VERSION);
	jest.advanceTimersByTime(6_000);
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
		await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS }).encode(peer.writer, VERSION);
		expect(await info).toMatchObject({ priority: 127 });
		expect(returned.closed.peek()).toBeUndefined();
	} else {
		jest.advanceTimersByTime(4_000);
		expect(await info).toBeInstanceOf(Error);
		expect(((await info) as Error).message).toContain("subscribe timed out after 10000ms");
	}
	expect(opening).toHaveBeenCalledTimes(1);
});

test("setup abandonment closes before a queued viewer can attach", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const broadcast = subscriber.consume(Path.from("room"));
	const pending = hooks.pendingTrackProducer;
	const resumed = Promise.withResolvers<track.Subscriber>();
	const captured = spyOn(hooks, "pendingTrackProducer").mockImplementationOnce((request) => {
		const producer = pending(request);
		const demand = producer.demand();
		const unused = demand.unused.bind(demand);
		spyOn(demand, "unused").mockImplementationOnce(async () => {
			await unused();
			const peek = demand.used.peek.bind(demand.used);
			let checks = 0;
			const observed = spyOn(demand.used, "peek").mockImplementation(() => {
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
		return producer;
	});
	const first = broadcast.track("video").subscribe();
	let returned: track.Subscriber | undefined;
	// Runner timeouts do not unwind an async test parked on the injection promise.
	onTestFinished(() => {
		captured.mockRestore();
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
	const request = await Subscribe.decode(next.reader, VERSION);
	await next.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS }).encode(next.writer, VERSION);
	expect(await info).toMatchObject({ priority: 127 });
});

test("abandonment cancels once and a late acceptance cannot capture a reused alias", async () => {
	const version = Version.DRAFT_16;
	const pair = createMockTransportPair(ALPN.DRAFT_16);
	const control = await Stream.open(pair.server, { version });
	const session = new ControlStreamAdapter(pair.server, control, version, 100n, true);
	void session.run().catch(() => void 0);
	const subscriber = new Subscriber({ session });
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing control stream");
	peer.reader.version = version;
	peer.writer.version = version;
	const broadcast = subscriber.consume(Path.from("room"));
	const pending = hooks.pendingTrackProducer;
	let producer: track.Producer | undefined;
	let oldRequest: track.Request | undefined;
	const captured = spyOn(hooks, "pendingTrackProducer").mockImplementationOnce((request) => {
		oldRequest = request;
		producer = pending(request);
		return producer;
	});
	const decoded = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	const decode = SubscribeOk.decode;
	const held = spyOn(SubscribeOk, "decode").mockImplementationOnce(async (...args) => {
		const ok = await decode(...args);
		decoded.resolve();
		await release.promise;
		return ok;
	});
	const first = broadcast.track("video").subscribe();
	let second: track.Subscriber | undefined;
	try {
		expect(await peer.reader.u53()).toBe(Subscribe.id);
		const request = await Subscribe.decode(peer.reader, version);
		if (!producer || !oldRequest) throw new Error("missing pending track");
		const accepted = spyOn(oldRequest, "accept");
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS }).encode(peer.writer, version);
		await decoded.promise;
		first.close();
		expect(await producer.closed).toBeInstanceOf(Error);
		expect(await peer.reader.u53()).toBe(Unsubscribe.id);
		expect((await Unsubscribe.decode(peer.reader, version)).requestId).toBe(request.requestId);

		second = broadcast.track("video").subscribe();
		// The next message must be the new request, not a duplicate UNSUBSCRIBE.
		expect(await peer.reader.u53()).toBe(Subscribe.id);
		const replacement = await Subscribe.decode(peer.reader, version);
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ requestId: replacement.requestId, trackAlias: ALIAS }).encode(peer.writer, version);
		await second.info();
		release.resolve();
		await subscriber.handleGroup(
			new GroupMessage({
				trackAlias: ALIAS,
				groupId: 0,
				subGroupId: 0,
				publisherPriority: 0,
				flags: groupFlags(true),
			}),
			new Reader(undefined, encodeObjects([0]), VERSION),
		);
		const groups = second.ordered();
		const group = await groups.nextGroup();
		expect(await group?.readString()).toBe("object 0");
		expect(accepted).not.toHaveBeenCalled();
		expect(second.closed.peek()).toBeUndefined();
		groups.close();
		accepted.mockRestore();
	} finally {
		release.resolve();
		held.mockRestore();
		captured.mockRestore();
		first.close();
		second?.close();
		broadcast.close();
		session.close();
	}
});

test("early group waits for SUBSCRIBE_OK priority before track acceptance", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const track = subscriber.consume(Path.from("room")).track("video").subscribe();
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing SUBSCRIBE stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, VERSION);

	const flags = { ...groupFlags(true), hasPriority: false };
	const group = new GroupMessage({ trackAlias: ALIAS, groupId: 3, subGroupId: 0, publisherPriority: 0, flags });
	const arriving = subscriber.handleGroup(group, new Reader(undefined, encodeObjects([0]), VERSION));
	const pending = await Promise.race([arriving.then(() => false), Promise.resolve(true)]);
	expect(pending).toBe(true);

	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		requestId: request.requestId,
		trackAlias: ALIAS,
		properties: { priority: 37 },
	}).encode(peer.writer, VERSION);
	await arriving;
	expect((await track.info()).priority).toBe(0xff - 37);
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
	expect((await track.info()).priority).toBe(0xff - 37);
	expect((await ordered.nextGroup())?.sequence).toBe(4);
	ordered.close();
	track.close();
});

/**
 * A group is the unit an application resyncs on, so one served from partway through is
 * unusable: the objects on the stream do not decode without the head the filter excluded,
 * and moq-lite cannot represent the hole at all. Delivering it would pass the group's sixth
 * frame off as the keyframe it opens with, so the group is dropped and the track resumes at
 * the next one. Our own publisher never opens such a stream; a draft-20 peer may.
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
 * Drafts 14-17 have no FIRST_OBJECT bit, so a subgroup that starts at the live edge
 * arrives with `firstObject` forced on and a non-zero first delta. That stream is the
 * in-progress group: drop it, keep the subscription, and deliver the next group, which
 * starts at object 0. A gap after an object was delivered still fails that group.
 */
test("a draft without FIRST_OBJECT drops a subgroup that starts mid-group", async () => {
	const version = Version.DRAFT_16;
	const { subscriber, track } = await subscribeTrack({ version });

	// The header cannot say otherwise on this draft: decode reports firstObject.
	const partial = new GroupMessage({
		trackAlias: ALIAS,
		groupId: 3,
		subGroupId: 0,
		publisherPriority: 0,
		flags: groupFlags(true),
	});
	await subscriber.handleGroup(partial, new Reader(undefined, encodeObjects([2, 0]), version));
	expect(track.latest()).toBeUndefined();
	expect(track.closed.peek()).toBeUndefined();

	const whole = groupFlags(true);
	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 4, subGroupId: 0, publisherPriority: 0, flags: whole }),
		new Reader(undefined, encodeObjects([0, 0]), version),
	);

	const ordered = track.ordered();
	const group = await ordered.nextGroup();
	expect(group?.sequence).toBe(4);
	expect(await group?.readString()).toBe("object 0");

	await subscriber.handleGroup(
		new GroupMessage({ trackAlias: ALIAS, groupId: 5, subGroupId: 0, publisherPriority: 0, flags: whole }),
		new Reader(undefined, encodeObjects([0, 5]), version),
	);
	const gapped = await ordered.nextGroup();
	expect(gapped?.sequence).toBe(5);
	expect(await gapped?.readString()).toBe("object 0");
	await expect(gapped?.readFrameSequence()).rejects.toThrow(/object IDs must start at 0/);
	expect(track.closed.peek()).toBeUndefined();

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

// Hold the actual legacy cancellation write so returning demand lands in the teardown gap.
test("returning demand survives a blocked unsubscribe", async () => {
	const version = Version.DRAFT_16;
	const pair = createMockTransportPair(ALPN.DRAFT_16);
	const control = await Stream.open(pair.server, { version });
	const session = new ControlStreamAdapter(pair.server, control, version, 100n, true);
	void session.run().catch(() => void 0);
	const subscriber = new Subscriber({ session });
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("missing control stream");
	peer.reader.version = version;
	peer.writer.version = version;

	const cancelStarted = Promise.withResolvers<void>();
	const releaseCancel = Promise.withResolvers<void>();
	const oldClosed = Promise.withResolvers<void>();
	const open = session.openBi.bind(session);
	const opening = spyOn(session, "openBi").mockImplementationOnce(() => {
		const stream = open();
		const write = stream.writer.u53.bind(stream.writer);
		spyOn(stream.writer, "u53").mockImplementation(async (value) => {
			if (value === Unsubscribe.id) {
				cancelStarted.resolve();
				await releaseCancel.promise;
			}
			await write(value);
		});
		const close = stream.close.bind(stream);
		spyOn(stream, "close").mockImplementation(() => {
			close();
			oldClosed.resolve();
		});
		return stream;
	});

	const broadcast = subscriber.consume(Path.from("room"));
	const first = broadcast.track("video").subscribe();
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, version);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS }).encode(peer.writer, version);

	// Receiving a group proves setup has accepted the track before demand disappears.
	await subscriber.handleGroup(
		new GroupMessage({
			trackAlias: ALIAS,
			groupId: 0,
			subGroupId: 0,
			publisherPriority: 0,
			flags: groupFlags(true),
		}),
		new Reader(undefined, encodeObjects([0]), VERSION),
	);
	const ordered = first.ordered();
	expect(await ordered.nextGroup()).toBeDefined();
	ordered.close();
	first.close();
	await cancelStarted.promise;
	const returned = broadcast.track("video").subscribe();
	releaseCancel.resolve();
	await oldClosed.promise;
	expect(returned.closed.peek()).toBeUndefined();
	expect(opening).toHaveBeenCalledTimes(2);
	returned.close();
	broadcast.close();
	session.close();
});

test("local readers filter hidden unsolicited namespaces from a legacy peer", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
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
