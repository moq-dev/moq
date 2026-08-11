import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestOk } from "./request.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

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
 * Every advertisement waits a round trip for the peer's reply. A broadcast published in
 * that window has to survive it: the loop is not watching the signal while it waits, so
 * a listener registered afterwards would sleep through the notification and leave the
 * namespace unadvertised until something unrelated changed.
 */
test("a broadcast published mid-advertisement is still announced", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const publisher = new Publisher(pair.server, session, { announce: false, interest: false });

	const first = new BroadcastProducer();
	publisher.publish(Path.from("first"), first);

	void publisher.runPublishNamespaces();

	// Take the first advertisement but withhold the reply, parking the loop.
	const one = await Stream.accept(pair.client, VERSION);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));

	// Publish while the loop is parked on that reply.
	const second = new BroadcastProducer();
	publisher.publish(Path.from("second"), second);
	await new Promise((resolve) => setTimeout(resolve, 5));

	await acceptPublishNamespace(one);

	const two = await Stream.accept(pair.client, VERSION);
	if (!two) throw new Error("the broadcast published mid-advertisement was never announced");
	expect(await readPublishNamespace(two)).toBe(Path.from("second"));
	await acceptPublishNamespace(two);

	publisher.close();
});
