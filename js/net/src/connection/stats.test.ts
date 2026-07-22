import { expect, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Time from "../time.ts";
import { accept, connect } from "./index.ts";
import { Reload } from "./reload.ts";
import { mergeStats, pollTransportStats, type TransportStats, transportStats } from "./stats.ts";

function fakeQuic(stats: TransportStats): WebTransport {
	return { getStats: () => Promise.resolve(stats) } as unknown as WebTransport;
}

// The poll refreshes every 100ms, so give it room to tick.
function settle(ms = 250): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

test("transportStats maps the W3C dictionary and normalizes a null send rate", async () => {
	const stats = await transportStats(
		fakeQuic({
			smoothedRtt: 25,
			estimatedSendRate: null,
			bytesSent: 1,
			bytesReceived: 2,
			bytesLost: 3,
			packetsSent: 4,
			packetsReceived: 5,
			packetsLost: 6,
		}),
	);
	expect(stats?.rtt).toBe(Time.Milli(25));
	expect(stats?.estimatedSendRate).toBeUndefined();
	expect(stats?.bytesSent).toBe(1);
	expect(stats?.bytesReceived).toBe(2);
	expect(stats?.bytesLost).toBe(3);
	expect(stats?.packetsSent).toBe(4);
	expect(stats?.packetsReceived).toBe(5);
	expect(stats?.packetsLost).toBe(6);

	// A snapshot without an RTT (e.g. a shim) leaves the field undefined.
	expect((await transportStats(fakeQuic({ bytesSent: 10 })))?.rtt).toBeUndefined();
});

test("transportStats returns undefined without getStats or when it rejects", async () => {
	expect(await transportStats({} as WebTransport)).toBeUndefined();

	const rejecting = { getStats: () => Promise.reject(new Error("nope")) } as unknown as WebTransport;
	expect(await transportStats(rejecting)).toBeUndefined();
});

test("mergeStats prefers the transport RTT and takes the receive rate from PROBE", () => {
	const transport = { rtt: Time.Milli(20), estimatedSendRate: 1_000, bytesSent: 5 };
	const probe = { rtt: Time.Milli(90), estimatedRecvRate: 2_000 };

	expect(mergeStats(transport, probe)).toEqual({
		rtt: Time.Milli(20),
		estimatedSendRate: 1_000,
		estimatedRecvRate: 2_000,
		bytesSent: 5,
	});

	// PROBE fills the RTT only when the transport doesn't measure one.
	expect(mergeStats({}, probe).rtt).toBe(Time.Milli(90));
	expect(mergeStats(transport, {}).rtt).toBe(Time.Milli(20));
	expect(mergeStats(transport, {}).estimatedRecvRate).toBeUndefined();
});

test("pollTransportStats refreshes until the connection closes", async () => {
	const stats: TransportStats = { smoothedRtt: 30, bytesSent: 1 };
	const closed = Promise.withResolvers<void>();
	const polled = pollTransportStats(fakeQuic(stats), closed.promise);

	await settle();
	expect(polled.peek().bytesSent).toBe(1);

	stats.bytesSent = 2;
	await settle();
	expect(polled.peek().bytesSent).toBe(2);

	closed.resolve();
	await settle();
	stats.bytesSent = 3;
	await settle();
	expect(polled.peek().bytesSent).toBe(2);
});

test("pollTransportStats keeps one request in flight and drops a stale completion", async () => {
	let calls = 0;
	let release!: (stats: TransportStats) => void;
	const quic = {
		getStats: () => {
			calls += 1;
			return new Promise<TransportStats>((resolve) => {
				release = resolve;
			});
		},
	} as unknown as WebTransport;

	const closed = Promise.withResolvers<void>();
	const polled = pollTransportStats(quic, closed.promise);

	// The first request never settles, so the timer must not start another.
	await settle(350);
	expect(calls).toBe(1);

	// Close, then let the outstanding request finish: its snapshot is stale.
	closed.resolve();
	release({ bytesSent: 7 });
	await settle();
	expect(polled.peek()).toEqual({});
	expect(calls).toBe(1);
});

test("pollTransportStats stays empty on a transport without getStats", async () => {
	const polled = pollTransportStats({} as WebTransport, new Promise(() => {}));
	await settle();
	expect(polled.peek()).toEqual({});
});

test("stats reports the transport snapshot over a live connection", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, {
		stats: { smoothedRtt: 40, estimatedSendRate: 2_000_000, bytesSent: 1_234, packetsLost: 3 },
	});
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		await settle();
		const stats = client.stats.peek();
		expect(stats.rtt).toBe(Time.Milli(40));
		expect(stats.estimatedSendRate).toBe(2_000_000);
		expect(stats.bytesSent).toBe(1_234);
		expect(stats.packetsLost).toBe(3);
		expect(stats.bytesReceived).toBeUndefined();
	} finally {
		client.close();
		server.close();
	}
});

test("stats stays empty on a transport without getStats", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	// Shadow the mock's getStats to imitate the qmux/WebSocket fallback.
	(pair.client as unknown as { getStats?: unknown }).getStats = undefined;
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		await settle();
		expect(client.stats.peek()).toEqual({});
	} finally {
		client.close();
		server.close();
	}
});

test("stats tracks the transport as it advances", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, { stats: { smoothedRtt: 40 } });
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		await settle();
		expect(client.stats.peek().rtt).toBe(Time.Milli(40));

		pair.client.stats.smoothedRtt = 80;
		await settle();
		expect(client.stats.peek().rtt).toBe(Time.Milli(80));
	} finally {
		client.close();
		server.close();
	}
});

test("Reload reports stats only while connected", async () => {
	const original = globalThis.WebTransport;
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, { stats: { smoothedRtt: 10 } });
	// `new` on a function returning an object yields that object, handing connect() the mock.
	const stub = function StubWebTransport() {
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	const url = new URL("https://example.com/");
	const reload = new Reload({ enabled: true, url, websocket: { enabled: false } });
	try {
		const server = await accept(pair.server, url);
		while (!reload.established.peek()) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
		await settle();
		expect(reload.stats.peek()?.rtt).toBe(Time.Milli(10));

		// Close the server side: the session dies within the initial delay, so the
		// loop escalates through the backoff and must not report the dead session.
		server.close();
		while (reload.established.peek()) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
		expect(reload.status.peek()).toBe("disconnected");
		await settle(0);
		expect(reload.stats.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("stats reports the transport snapshot on an IETF connection", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18, {
		stats: { smoothedRtt: 15, bytesReceived: 42 },
	});
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		await settle();
		const stats = client.stats.peek();
		expect(stats.rtt).toBe(Time.Milli(15));
		expect(stats.bytesReceived).toBe(42);
		// moq-transport has no PROBE, so there is no receive estimate.
		expect(stats.estimatedRecvRate).toBeUndefined();
	} finally {
		client.close();
		server.close();
	}
});
