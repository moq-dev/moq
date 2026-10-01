import { expect, test } from "bun:test";
import { createMockTransportPair } from "../mock.ts";
import { Stream } from "../stream.ts";
import * as Time from "../time.ts";
import { wireOf } from "../wire.ts";
import { Connection } from "./connection.ts";
import { GoAway } from "./goaway.ts";
import { ALPN, Version } from "./version.ts";

// Draft-17+ carry GOAWAY on the setup stream, with the peer's deadline. The session keeps
// serving afterwards so its groups in flight finish while the caller migrates.
test("a draft-17 GOAWAY surfaces its URI and deadline without closing the session", async () => {
	const version = Version.DRAFT_17;
	const pair = createMockTransportPair(ALPN.DRAFT_17);
	const control = await Stream.open(pair.client, { version });
	const peer = await Stream.accept(pair.server, version);
	if (!peer) throw new Error("no setup stream");

	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		control,
		maxRequestId: 100n,
		version,
		client: true,
	});

	let closed = false;
	void connection.closed.then(() => {
		closed = true;
	});

	try {
		await peer.writer.u53(GoAway.id);
		await new GoAway({ newSessionUri: "", timeout: 5000n }).encode(peer.writer, version);

		const drain = await wireOf(connection).goaway;
		expect(drain.uri).toBe("");
		expect(drain.timeout).toBe(Time.Milli(5000));

		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(closed).toBe(false);
	} finally {
		connection.close();
	}
});

test("a server rejects a client GOAWAY that names a redirect", async () => {
	const version = Version.DRAFT_17;
	const pair = createMockTransportPair(ALPN.DRAFT_17);
	const control = await Stream.open(pair.client, { version });
	const peer = await Stream.accept(pair.server, version);
	if (!peer) throw new Error("no setup stream");

	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		control,
		maxRequestId: 100n,
		version,
		client: false,
	});

	let closed = false;
	void connection.closed.then(() => {
		closed = true;
	});

	try {
		await peer.writer.u53(GoAway.id);
		await new GoAway({ newSessionUri: "https://other.example/", timeout: 0n }).encode(peer.writer, version);
		await new Promise((resolve) => setTimeout(resolve, 50));
		expect(closed).toBe(true);
	} finally {
		connection.close();
	}
});

test("a zero GOAWAY timeout reads as no deadline", async () => {
	const msg = new GoAway({ newSessionUri: "https://relay.example/next", timeout: 0n });
	expect(msg.drain()).toEqual({ uri: "https://relay.example/next", timeout: undefined });
});
