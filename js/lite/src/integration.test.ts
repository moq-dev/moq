import assert from "node:assert";
import test from "node:test";
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
	assert.ok(entry);
	assert.strictEqual(entry.path, "test");
	assert.strictEqual(entry.active, true);

	// Client consumes the broadcast and subscribes to a track
	const remote = client.consume(Path.from("test"));
	const track = remote.subscribe("video", 0);

	// Server handles the subscription request
	const req = await broadcast.requested();
	assert.ok(req);
	assert.strictEqual(req.track.name, "video");

	// Server writes data
	req.track.writeString("hello");

	// Client reads data
	const data = await track.readString();
	assert.strictEqual(data, "hello");

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
	await assert.rejects(async () => {
		await track.readString();
	});

	client.close();
	server.close();
});
