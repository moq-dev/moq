import { expect, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Time from "../time.ts";
import type { Established } from "./established.ts";
import { accept, connect } from "./index.ts";
import { Reload } from "./reload.ts";
import { type ConnectionStats, type TransportStats, transportStats } from "./stats.ts";

// Both built-in connections implement the optional stats().
async function snapshot(connection: Established): Promise<ConnectionStats> {
	if (!connection.stats) throw new Error("stats not implemented");
	return connection.stats();
}

function fakeQuic(stats: TransportStats): WebTransport {
	return { getStats: () => Promise.resolve(stats) } as unknown as WebTransport;
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

test("stats reports the transport snapshot over a live connection", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, {
		stats: { smoothedRtt: 40, estimatedSendRate: 2_000_000, bytesSent: 1_234, packetsLost: 3 },
	});
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		const stats = await snapshot(client);
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

test("stats falls back to the PROBE fields on a transport without getStats", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	// Shadow the mock's getStats to imitate the qmux/WebSocket fallback.
	(pair.client as unknown as { getStats?: unknown }).getStats = undefined;
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		client.rtt?.set(Time.Milli(70));
		const stats = await snapshot(client);
		expect(stats.rtt).toBe(Time.Milli(70));
		expect(stats.bytesSent).toBeUndefined();
		expect(stats.estimatedSendRate).toBeUndefined();
	} finally {
		client.close();
		server.close();
	}
});

test("the stats poll feeds the rtt signal from the transport", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, { stats: { smoothedRtt: 40 } });
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		// The poll runs every 100 ms; give it two ticks.
		await new Promise((resolve) => setTimeout(resolve, 250));
		expect(client.rtt?.peek()).toBe(Time.Milli(40));
	} finally {
		client.close();
		server.close();
	}
});

test("a session that dies during reconnect backoff reports disconnected", async () => {
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
		expect(await reload.stats()).toBeDefined();

		// Close the server side: the session dies within the initial delay, so the
		// loop escalates through the backoff and must not report the dead session.
		server.close();
		while (reload.established.peek()) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
		expect(reload.status.peek()).toBe("disconnected");
		expect(await reload.stats()).toBeUndefined();
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
		const stats = await snapshot(client);
		expect(stats.rtt).toBe(Time.Milli(15));
		expect(stats.bytesReceived).toBe(42);
		// moq-transport has no PROBE, so there is no receive estimate.
		expect(stats.estimatedRecvRate).toBeUndefined();
	} finally {
		client.close();
		server.close();
	}
});
