import { expect, test } from "bun:test";
import { Broadcast } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import * as Path from "./path.ts";

const url = new URL("https://localhost:4443/test");

async function runPublishSubscribeFlow(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, version !== undefined ? { version } : undefined),
	]);

	// Server publishes a broadcast
	const broadcast = new Broadcast();
	server.publish(Path.from("test"), broadcast);

	// Client discovers announced broadcast
	const announced = client.announced();
	const entry = await announced.next();
	if (!entry) throw new Error("expected entry");
	expect(entry.path).toBe("test" as Path.Valid);
	expect(entry.active).toBe(true);

	// Client consumes the broadcast and subscribes to a track
	const remote = client.consume(Path.from("test"));
	const track = remote.subscribe("video", 0);

	// Server handles the subscription request
	const req = await broadcast.requested();
	if (!req) throw new Error("expected req");
	expect(req.track.name).toBe("video");

	// Server writes data
	req.track.writeString("hello");

	// Client reads data
	const data = await track.readString();
	expect(data).toBe("hello");

	// Cleanup
	req.track.close();
	broadcast.close();
	announced.close();
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

// Regression: on the multiplexed control-stream drafts (14-16) a subscribe must
// resolve even when it races an inbound announce, without first "warming" the
// session by reading the announce. The client and server share one virtual-stream
// routing map keyed by request ID, so if both peers allocated from the same ID
// space the inbound PUBLISH_NAMESPACE would clobber the pending SUBSCRIBE's slot
// and SUBSCRIBE_OK would be delivered to the wrong stream, hanging forever.
async function runSubscribeWithoutWarmup(version: number) {
	const pair = createMockTransportPair("");

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version }),
	]);

	const broadcast = new Broadcast();
	server.publish(Path.from("test"), broadcast);
	const serving = (async () => {
		const req = await broadcast.requested();
		if (req) req.track.writeString("hello");
	})();

	// Subscribe immediately, without awaiting the announce.
	const remote = client.consume(Path.from("test"));
	const track = remote.subscribe("video", 0);

	const data = await Promise.race([
		track.readString(),
		new Promise<never>((_, reject) =>
			setTimeout(() => reject(new Error("timed out waiting for SUBSCRIBE_OK")), 2000),
		),
	]);
	expect(data).toBe("hello");

	await serving;
	broadcast.close();
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

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, { version: Ietf.Version.DRAFT_14 }),
	]);

	// Client tries to consume a broadcast that nobody is publishing
	const remote = client.consume(Path.from("nonexistent"));
	const track = remote.subscribe("video", 0);

	// Reading should eventually error since the broadcast doesn't exist
	await expect(
		(async () => {
			await track.readString();
		})(),
	).rejects.toThrow();

	client.close();
	server.close();
});
