import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** Accept the next stream the publisher opens, or give up rather than hang forever. */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
	return Promise.race([
		Stream.accept(transport, VERSION),
		new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), 1000)),
	]);
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
	return new Publisher(transport, session, { announce: false, interest: false });
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
	await new Promise((resolve) => setTimeout(resolve, 5));

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
