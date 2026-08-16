import { expect, test } from "bun:test";
import type * as announce from "../announced.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { RequestOk } from "./request.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

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

	const subscriber = new Subscriber(session);
	subscriber.announced(Path.empty());

	expect(await nextStream(pair.client)).toBeDefined();
});

/**
 * The other half of discovery: a peer that tells us unasked. Asking must not make us deaf
 * to a PUBLISH_NAMESPACE that arrives on its own stream instead.
 */
test("an unsolicited announcement lands", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());

	// The question we asked, which this peer never answers.
	expect(await nextStream(pair.client)).toBeDefined();

	// What the connection dispatch does when a PUBLISH_NAMESPACE arrives instead.
	const stream = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("surprise") }),
		stream,
	);

	const next = await announced.next();
	expect(next?.path).toBe(Path.from("surprise"));
	expect(next?.active).toBe(true);

	// The handler holds the request open until the peer drops it, and withdraws the
	// namespace on the way out.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await handler;
	expect(await announced.next()).toMatchObject({ path: Path.from("surprise"), active: false });
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
async function inlineNamespace(stream: Stream, path: Path.Valid): Promise<void> {
	await stream.writer.u53(SubscribeNamespaceEntry.id);
	await new SubscribeNamespaceEntry({ suffix: path }).encode(stream.writer, VERSION);
}

/**
 * Advertise a path nothing else uses and wait for it, which proves the entries written
 * before it on the same stream have been read. Announcing an already-announced path is
 * silent by design, so it cannot be waited on directly.
 */
async function syncInline(stream: Stream, announced: announce.Consumer): Promise<void> {
	await inlineNamespace(stream, Path.from("sentinel"));
	expect(await announced.next()).toMatchObject({ path: Path.from("sentinel"), active: true });
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
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Unsolicited first, then the same path inline on the subscription.
	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("both") }),
		request,
	);
	expect(await announced.next()).toMatchObject({ path: Path.from("both"), active: true });

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
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());
	const subscription = await acceptSubscribeNamespace(pair.client);

	const request = await Stream.open(pair.server, { version: VERSION });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("both") }),
		request,
	);
	expect(await announced.next()).toMatchObject({ path: Path.from("both"), active: true });

	await inlineNamespace(subscription, Path.from("both"));
	await syncInline(subscription, announced);

	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await handler;

	// Now the subscription drops it too, which is the last reference.
	await subscription.writer.u53(SubscribeNamespaceEntryDone.id);
	await new SubscribeNamespaceEntryDone({ suffix: Path.from("both") }).encode(subscription.writer, VERSION);

	expect(await announced.next()).toMatchObject({ path: Path.from("both"), active: false });
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
	const subscriber = new Subscriber(session);

	// Two feeds, so one outlives the stream that carries the advertisement.
	const doomed = subscriber.announced(Path.empty());
	const streamA = await acceptSubscribeNamespace(pair.client);
	const survivor = subscriber.announced(Path.empty());
	await acceptSubscribeNamespace(pair.client);

	await inlineNamespace(streamA, Path.from("orphan"));
	expect(await doomed.next()).toMatchObject({ path: Path.from("orphan"), active: true });
	expect(await survivor.next()).toMatchObject({ path: Path.from("orphan"), active: true });

	// The stream that advertised it goes away without a NAMESPACE_DONE.
	streamA.writer.close();

	expect(await survivor.next()).toMatchObject({ path: Path.from("orphan"), active: false });
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
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());

	const first = await Stream.open(pair.server, { version: Version.DRAFT_15 });
	const handler = subscriber.runPublishNamespace(
		new PublishNamespace({ requestId: 0n, trackNamespace: Path.from("twice") }),
		first,
	);
	expect(await announced.next()).toMatchObject({ path: Path.from("twice"), active: true });

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

	expect(await announced.next()).toMatchObject({ path: Path.from("twice"), active: false });
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
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());
	const subscription = await acceptSubscribeNamespace(pair.client);

	// Both entries land in one chunk, so the second is buffered while the first is being
	// delivered. Closing the consumer then races the loop that would attach it.
	await inlineNamespace(subscription, Path.from("first"));
	await inlineNamespace(subscription, Path.from("buffered"));

	announced.close();

	// Whatever the loop managed to attach, the stream gave back on its way out.
	await new Promise((resolve) => setTimeout(resolve, 50));

	const fresh = subscriber.announced(Path.empty());
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
	const subscriber = new Subscriber(session);

	const announced = subscriber.announced(Path.empty());

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

	expect(await announced.next()).toMatchObject({ path: Path.from("raced"), active: true });
	await two;

	// Only one reference was taken, so the surviving request ending retracts the path.
	const peer = await nextStream(pair.client);
	if (!peer) throw new Error("no PUBLISH_NAMESPACE stream to close");
	peer.close();
	await one;

	expect(await announced.next()).toMatchObject({ path: Path.from("raced"), active: false });
});
