import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession, type Session } from "./adapter.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
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
 */
async function declinePublishNamespace(stream: Stream): Promise<void> {
	try {
		await stream.writer.u53(RequestError.id);
		await new RequestError({
			requestId: undefined,
			errorCode: 403,
			reasonPhrase: "no",
			retryInterval: 0n,
		}).encode(stream.writer, VERSION);
	} catch {
		// The publisher reset the request out from under us.
	}
}

function publisher(transport: WebTransport): Publisher {
	const session = new NativeSession(transport, VERSION, true);
	return new Publisher(transport, session, { announce: false });
}

/**
 * Every advertisement waits a round trip for the peer's reply. A broadcast published in
 * that window has to survive it: the loop is not watching the signal while it waits, so
 * a listener registered afterwards would sleep through the notification and leave the
 * namespace unadvertised until something unrelated changed.
 */
test("a broadcast published mid-advertisement is still announced", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);

	const first = new BroadcastProducer();
	pub.publish(Path.from("first"), first);

	void pub.runPublishNamespaces();

	// Take the first advertisement but withhold the reply, parking the loop.
	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));

	// Publish while the loop is parked on that reply.
	const second = new BroadcastProducer();
	pub.publish(Path.from("second"), second);
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	await acceptPublishNamespace(one);

	const two = await nextStream(pair.client);
	if (!two) throw new Error("the broadcast published mid-advertisement was never announced");
	expect(await readPublishNamespace(two)).toBe(Path.from("second"));
	await acceptPublishNamespace(two);

	pub.close();
});

/**
 * A peer may decline an advertisement and stay connected. Recording it as advertised
 * anyway would strand the namespace: nothing re-adds it to the diff, so it would never
 * be offered again for the life of the session.
 */
test("a declined advertisement is retried on the next change", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);

	const first = new BroadcastProducer();
	pub.publish(Path.from("first"), first);

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(declined)).toBe(Path.from("first"));
	await declinePublishNamespace(declined);

	// Any later change re-runs the diff, which is where the refused namespace has to
	// reappear rather than being remembered as up.
	const second = new BroadcastProducer();
	pub.publish(Path.from("second"), second);

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("second"));
	expect(seen).toContain(Path.from("first"));

	pub.close();
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

	const pub = new Publisher(pair.server, session, { announce: false });
	pub.publish(Path.from("first"), new BroadcastProducer());

	void pub.runPublishNamespaces();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	// The refused open cost "first" its turn; the next change has to bring it back along
	// with the newcomer.
	pub.publish(Path.from("second"), new BroadcastProducer());

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("first"));
	expect(seen).toContain(Path.from("second"));

	pub.close();
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

	const pub = new Publisher(pair.server, session, { announce: false });
	pub.publish(Path.from("lonely"), new BroadcastProducer());

	void pub.runPublishNamespaces();

	// Nothing else happens: no second publish, no close. Only the retry can save it.
	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the refused namespace was never retried");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await acceptPublishNamespace(stream);

	pub.close();
});
