import { expect, test } from "bun:test";
import { type Origin, OriginSchema } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession, type Session } from "./adapter.ts";
import type * as Cluster from "./cluster.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { SubscribeNamespace } from "./subscribe_namespace.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** How long to wait for a stream before calling it absent, which is how a regression reports. */
const STREAM_WAIT = 1000;

/** Long enough for the publish to reach the announce loop's signal. */
const SETTLE = 5;

/**
 * Accept the next stream the publisher opens, or give up rather than hang forever.
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

/** Read one PUBLISH_NAMESPACE off a stream the publisher opened. */
async function readPublishNamespace(stream: Stream): Promise<Path.Valid> {
	const typeId = await stream.reader.u53();
	expect(typeId).toBe(PublishNamespace.id);

	const msg = await PublishNamespace.decode(stream.reader, VERSION);
	return msg.trackNamespace;
}

/** Answer a PUBLISH_NAMESPACE, which is what unblocks the announce loop. */
async function acceptPublishNamespace(stream: Stream): Promise<void> {
	await stream.writer.u53(RequestOk.id);
	await new RequestOk({ requestId: undefined }).encode(stream.writer, VERSION);
}

/**
 * Decline a PUBLISH_NAMESPACE, which the peer is allowed to do without ending the
 * session. The publisher resets the request as soon as it reads the type, which lands
 * back here as a write error once the refusal is already on the wire.
 *
 * `retryInterval` is what the peer says about coming back, in milliseconds: 0 asks not to
 * be offered the namespace again, and anything else is a minimum wait.
 */
async function declinePublishNamespace(stream: Stream, retryInterval = 1n): Promise<void> {
	try {
		await stream.writer.u53(RequestError.id);
		await new RequestError({
			requestId: undefined,
			errorCode: 403,
			reasonPhrase: "no",
			retryInterval,
		}).encode(stream.writer, VERSION);
	} catch {
		// The publisher reset the request out from under us.
	}
}

/**
 * A publisher serving `origin`, which is what a session publishes from: the broadcasts
 * are the origin's, so a test publishes and unpublishes through it rather than the
 * publisher.
 */
function publisher(
	transport: WebTransport,
	{
		requiresSolicitation = false,
		session,
		cluster,
	}: { requiresSolicitation?: boolean; session?: Session; cluster?: Cluster.Hops } = {},
): { pub: Publisher; origin: OriginProducer } {
	const origin = new OriginProducer();
	const inner = session ?? new NativeSession(transport, VERSION, true);
	return {
		pub: new Publisher({
			quic: transport,
			session: inner,
			publish: origin.consume(),
			requiresSolicitation,
			cluster,
		}),
		origin,
	};
}

/**
 * Every advertisement waits a round trip for the peer's reply. A broadcast published in
 * that window has to survive it: the loop is not watching the signal while it waits, so
 * a listener registered afterwards would sleep through the notification and leave the
 * namespace unadvertised until something unrelated changed.
 */
test("a broadcast published mid-advertisement is still announced", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);

	origin.publish(Path.from("first"));

	void pub.runPublishNamespaces();

	// Take the first advertisement but withhold the reply, parking the loop.
	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));

	// Publish while the loop is parked on that reply.
	origin.publish(Path.from("second"));
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	await acceptPublishNamespace(one);

	const two = await nextStream(pair.client);
	if (!two) throw new Error("the broadcast published mid-advertisement was never announced");
	expect(await readPublishNamespace(two)).toBe(Path.from("second"));
	await acceptPublishNamespace(two);

	origin.close();
});

/**
 * A peer may decline an advertisement and stay connected. Recording it as advertised
 * anyway would strand the namespace: nothing re-adds it to the diff, so it would never
 * be offered again for the life of the session.
 */
test("a declined advertisement is retried on the next change", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);

	origin.publish(Path.from("first"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(declined)).toBe(Path.from("first"));
	await declinePublishNamespace(declined);

	// Any later change re-runs the diff, which is where the refused namespace has to
	// reappear rather than being remembered as up.
	origin.publish(Path.from("second"));

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("second"));
	expect(seen).toContain(Path.from("first"));

	origin.close();
});

/**
 * A peer out of stream credit rejects the open. That has to cost the namespace a turn,
 * not the session its discovery: the announce loop is never restarted, so unwinding it
 * would lose every future publish too.
 */
test("a failed stream open does not kill the announce loop", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, VERSION, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	const { pub, origin } = publisher(pair.server, { session });
	origin.publish(Path.from("first"));

	void pub.runPublishNamespaces();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	// The refused open cost "first" its turn; the next change has to bring it back along
	// with the newcomer.
	origin.publish(Path.from("second"));

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("first"));
	expect(seen).toContain(Path.from("second"));

	origin.close();
});

/**
 * Capacity coming back raises no signal of its own: no broadcast is published, closed, or
 * changed. The loop has to come back and ask again on its own, or a namespace refused
 * once stays undiscoverable for the session.
 */
test("a namespace refused once is retried without anything else changing", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, VERSION, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	const { pub, origin } = publisher(pair.server, { session });
	origin.publish(Path.from("lonely"));

	void pub.runPublishNamespaces();

	// Nothing else happens: no second publish, no close. Only the retry can save it.
	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the refused namespace was never retried");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await acceptPublishNamespace(stream);

	origin.close();
});

/**
 * The solicited legacy path advertises with PUBLISH_NAMESPACE requests too, so a declined
 * one needs the same retry the unsolicited loop has. Without it, a namespace refused once
 * stays undiscoverable for the life of the subscription, since the peer starting to
 * answer raises no signal the loop is watching.
 */
test("a solicited legacy advertisement refused once is retried", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, Version.DRAFT_15, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	// The peer declared that advertisements to it must be solicited, so this is the loop
	// that answers its SUBSCRIBE_NAMESPACE.
	const { pub, origin } = publisher(pair.server, { requiresSolicitation: true, session });
	origin.publish(Path.from("lonely"));

	const subscription = await Stream.open(pair.client, { version: Version.DRAFT_15 });
	const accepted = await Stream.accept(pair.server, Version.DRAFT_15);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), accepted);

	// Nothing else happens: no second publish, no close. Only the retry can save it.
	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the refused namespace was never retried");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await acceptPublishNamespace(stream);

	subscription.close();
	origin.close();
});

/**
 * A peer that refuses an advertisement with a retry interval of 0 is asking not to be
 * offered it again. Coming back anyway turns a permanent refusal (unauthorized,
 * uninterested) into a request every few seconds for the life of the session.
 */
test("a refusal that forbids retrying is not retried", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);
	origin.publish(Path.from("lonely"));

	void pub.runPublishNamespaces();

	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await declinePublishNamespace(stream, 0n);

	// Well past the retry the loop would otherwise take.
	expect(await nextStream(pair.client)).toBeUndefined();

	origin.close();
});

/**
 * The same rule with no gap to observe: republishing a path swaps the routing front in one
 * mutation, so the path never leaves the origin's map and only the front says the peer is
 * being offered a different broadcast. Keying the refusal on the path alone would strand
 * the replacement for the life of the session.
 */
test("republishing a path clears a refusal without unannouncing first", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);

	origin.publish(Path.from("recycled"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("recycled"));
	await declinePublishNamespace(declined, 0n);
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	// A new broadcast takes the path over outright: no close, no gap.
	origin.publish(Path.from("recycled"));

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("the replacement broadcast was never offered");
	expect(await readPublishNamespace(retried)).toBe(Path.from("recycled"));
	await acceptPublishNamespace(retried);

	origin.close();
});

/**
 * A refusal belongs to the namespace, not the path forever. Unannouncing takes it with
 * it, so a fresh broadcast at the same path is offered again; keeping it would strand
 * that path for the life of the session with no timer able to recover it. Rust gets this
 * by rebuilding the watched entry on re-announce.
 */
test("re-announcing a path clears a refusal that forbade retrying", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);

	const first = origin.publish(Path.from("recycled"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("recycled"));
	await declinePublishNamespace(declined, 0n);

	// The broadcast goes away, taking the refusal with it, and a new one takes its place.
	first.close();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));
	origin.publish(Path.from("recycled"));

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("a re-announced path was never offered again");
	expect(await readPublishNamespace(retried)).toBe(Path.from("recycled"));
	await acceptPublishNamespace(retried);

	origin.close();
});

/**
 * The origin outlives the session and its signal never ends, so a closed connection
 * reaches this loop through nothing it watches. Left unbounded it parks on the shared
 * origin forever, waking on an unrelated publish to fail against a dead transport.
 */
test("closing the session ends the unsolicited announce loop", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const { pub, origin } = publisher(pair.server);

	origin.publish(Path.from("first"));

	const loop = pub.runPublishNamespaces();

	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));
	await acceptPublishNamespace(one);

	// The session ends. The origin is untouched: it is shared, and other sessions keep using it.
	pair.server.close();

	await Promise.race([
		loop,
		new Promise((_resolve, reject) =>
			setTimeout(() => reject(new Error("the announce loop outlived its session")), STREAM_WAIT),
		),
	]);

	// The origin really did survive the session, so publishing into it is still valid.
	origin.publish(Path.from("second"));
	origin.close();
});

/**
 * A peer that declared a Hop ID gets one on every advertisement: it is what lets the peer
 * tell that an advertisement it hears back came from us. A peer that declared nothing has
 * not read ours either, so sending it the parameters would be a protocol violation.
 */
test("an advertisement carries our hop id once the peer declared one", async () => {
	const self: Origin = OriginSchema.parse(7n);

	for (const peer of [OriginSchema.parse(9n), undefined]) {
		const pair = createMockTransportPair(ALPN.DRAFT_19);
		const { pub, origin } = publisher(pair.server, { cluster: { self, peer } });
		origin.publish(Path.from("mine"));
		void pub.runPublishNamespaces();

		const stream = await nextStream(pair.client);
		if (!stream) throw new Error("no PUBLISH_NAMESPACE for the broadcast");
		expect(await stream.reader.u53()).toBe(PublishNamespace.id);

		if (peer === undefined) {
			// Nothing negotiated, so the parameters are absent: reading the message as a
			// negotiated one finds no HOP_PATH and rejects.
			await expect(PublishNamespace.decode(stream.reader, VERSION, true)).rejects.toThrow();
		} else {
			// Our own Hop ID is the last entry, and we originate everything we advertise,
			// so it is the only one. The cost is 0: we are already producing the content.
			const msg = await PublishNamespace.decode(stream.reader, VERSION, true);
			expect(msg.trackNamespace).toBe(Path.from("mine"));
			expect(msg.cluster).toEqual({ hops: [self], cost: 0n });
			await acceptPublishNamespace(stream);
		}

		origin.close();
	}
});
