import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";

const url = new URL("https://localhost:4443/test");

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

async function runPublishSubscribeFlow(protocol: string, version?: number) {
	const pair = createMockTransportPair(protocol);

	const [client, server] = await Promise.all([
		connect(url, { transport: pair.client }),
		accept(pair.server, url, version !== undefined ? { version } : undefined),
	]);

	// Server publishes a broadcast
	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
	const prefixedBroadcast = new BroadcastProducer();
	server.publish(Path.from("root/child"), prefixedBroadcast);

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

	// Prefix-scoped discovery returns paths relative to the requested prefix.
	const prefixed = client.announced(Path.from("root"));
	const prefixedEntry = await prefixed.next();
	if (!prefixedEntry) throw new Error("expected prefixed entry");
	expect(prefixedEntry.path).toBe("child" as Path.Valid);
	expect(prefixedEntry.active).toBe(true);

	// Client consumes the broadcast and subscribes to a track
	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Client reads data
	const data = await track.readString();
	expect(data).toBe("hello");
	expect(served).toBeGreaterThan(0);

	// Cleanup
	broadcast.close();
	prefixedBroadcast.close();
	await serving;
	announced.close();
	prefixed.close();
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

test("integration: lite draft-05", async () => {
	// Exercises AnnounceOk: the announce flow only completes if the subscriber
	// reads the publisher's AnnounceOk before the initial Announce messages.
	await runPublishSubscribeFlow(Lite.ALPN_05);
});

test("integration: lite draft-05 datagram delivery", async () => {
	const enc = new TextEncoder();
	const dec = new TextDecoder();
	const pair = createMockTransportPair(Lite.ALPN_05);

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	// A static track fans datagrams out to whoever subscribes.
	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Datagrams aren't cached, so the first few may race the subscription setup. Pump until
	// the subscriber receives one, then stop.
	const received = track.recvDatagram();
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			producer.appendDatagram(Timestamp.fromMillis(i), enc.encode("dgram"));
			await sleep(2);
		}
	})();

	const got = await received;
	stop = true;
	await pump;

	expect(got).toBeDefined();
	expect(dec.decode(got?.payload)).toBe("dgram");

	broadcast.close();
	remote.close();
	client.close();
	server.close();
});

test("integration: lite draft-05 datagrams not sent on a non-datagram transport", async () => {
	const enc = new TextEncoder();
	// maxDatagramSize 0 simulates a qmux/WebSocket session: the publisher must fall back to
	// not sending datagrams (there is no group fallback), while groups still flow.
	const pair = createMockTransportPair(Lite.ALPN_05, { datagrams: false });

	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);

	const broadcast = new BroadcastProducer();
	server.publish(Path.from("test"), broadcast);
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = client.consume(Path.from("test"));
	const track = remote.track("video").subscribe();

	// Keep pushing a group (to prove the connection is live) and a datagram (which must be dropped).
	let stop = false;
	const pump = (async () => {
		for (let i = 0; !stop; i++) {
			producer.appendDatagram(Timestamp.fromMillis(i), enc.encode("dgram"));
			producer.writeString("group");
			await sleep(2);
		}
	})();

	// A group arrives, confirming the subscription works over this transport.
	const grp = await track.readString();
	expect(grp).toBe("group");

	// No datagram is ever delivered: recvDatagram stays pending until the timeout wins.
	const datagram = track.recvDatagram();
	datagram.catch(() => {}); // The track close below settles it; swallow to avoid a stray rejection.
	const outcome = await Promise.race([datagram, sleep(50).then(() => "timeout" as const)]);
	expect(outcome).toBe("timeout");

	stop = true;
	await pump;

	broadcast.close();
	remote.close();
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

test("integration: ietf draft-19", async () => {
	await runPublishSubscribeFlow(Ietf.ALPN.DRAFT_19);
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
