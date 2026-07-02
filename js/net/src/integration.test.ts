import { expect, test } from "bun:test";
import { Broadcast } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import * as Path from "./path.ts";
import { Timestamp } from "./time.ts";

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

	// Client consumes the broadcast and subscribes to a track
	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Client reads data
	const data = await track.readString();
	expect(data).toBe("hello");
	expect(served).toBeGreaterThan(0);

	// Cleanup
	broadcast.close();
	await serving;
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

test("integration: lite draft-05-wip", async () => {
	// Exercises AnnounceOk: the announce flow only completes if the subscriber
	// reads the publisher's AnnounceOk before the initial Announce messages.
	await runPublishSubscribeFlow(Lite.ALPN_05_WIP);
});

test("integration: lite draft-05-wip fetches a cached group", async () => {
	const pair = createMockTransportPair(Lite.ALPN_05_WIP);
	const enc = new TextEncoder();
	const dec = new TextDecoder();

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new Broadcast();
	const producer = broadcast.createTrack("video");
	server.publish(Path.from("test"), broadcast);

	const group0 = producer.appendGroup();
	group0.writeFrame({ data: enc.encode("alpha"), timestamp: Timestamp.fromMillis(10) });
	group0.writeFrame({ data: enc.encode("beta"), timestamp: Timestamp.fromMillis(15) });
	group0.close();

	const group1 = producer.appendGroup();
	group1.writeFrame({ data: enc.encode("newer"), timestamp: Timestamp.fromMillis(20) });
	group1.close();

	const remote = client.consume(Path.from("test"));
	const fetched = await remote.track("video").fetchGroup(0);

	const first = await fetched.readFrame();
	expect(dec.decode(first?.data)).toBe("alpha");
	expect(first?.timestamp.asMillis()).toBe(10);

	const second = await fetched.readFrame();
	expect(dec.decode(second?.data)).toBe("beta");
	expect(second?.timestamp.asMillis()).toBe(15);
	expect(await fetched.readFrame()).toBeUndefined();

	remote.close();
	broadcast.close();
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

test("integration: ietf fetch group is explicitly unsupported", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18);

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const remote = client.consume(Path.from("test"));
	await expect(remote.track("video").fetchGroup(0)).rejects.toThrow("fetch group is not supported for moq-transport");

	remote.close();
	client.close();
	server.close();
});
