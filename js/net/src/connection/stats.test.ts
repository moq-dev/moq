import { expect, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Time from "../time.ts";
import { accept, connect } from "./index.ts";
import { Reload } from "./reload.ts";
import { type TransportStats, transportStats } from "./stats.ts";

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
	expect(stats.rtt).toBe(Time.Milli(25));
	expect(stats.estimatedSendRate).toBeUndefined();
	expect(stats.bytesSent).toBe(1);
	expect(stats.bytesReceived).toBe(2);
	expect(stats.bytesLost).toBe(3);
	expect(stats.packetsSent).toBe(4);
	expect(stats.packetsReceived).toBe(5);
	expect(stats.packetsLost).toBe(6);

	// Browsers don't report an RTT today, so the field is usually absent.
	expect((await transportStats(fakeQuic({ bytesSent: 10 }))).rtt).toBeUndefined();
});

test("transportStats returns an empty snapshot without getStats or when it rejects", async () => {
	expect(await transportStats({} as WebTransport)).toEqual({});

	const rejecting = { getStats: () => Promise.reject(new Error("nope")) } as unknown as WebTransport;
	expect(await transportStats(rejecting)).toEqual({});
});

test("stats() snapshots the transport on demand", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, {
		stats: { estimatedSendRate: 2_000_000, bytesSent: 1_234, packetsLost: 3 },
	});
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		const first = await client.stats();
		expect(first.estimatedSendRate).toBe(2_000_000);
		expect(first.bytesSent).toBe(1_234);
		expect(first.packetsLost).toBe(3);
		expect(first.bytesReceived).toBeUndefined();

		// Each call queries the transport again, so a caller controls its own sampling.
		pair.client.stats.bytesSent = 5_678;
		expect((await client.stats()).bytesSent).toBe(5_678);
	} finally {
		client.close();
		server.close();
	}
});

test("stats() is empty on a transport without getStats", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06_WIP);
	// Shadow the mock's getStats to imitate the qmux/WebSocket fallback.
	(pair.client as unknown as { getStats?: unknown }).getStats = undefined;
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		expect(await client.stats()).toEqual({});
	} finally {
		client.close();
		server.close();
	}
});

test("probe starts empty and stays empty without PROBE support", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18, { stats: { bytesReceived: 42 } });
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		// moq-transport has no PROBE, but the transport counters still work.
		expect(client.probe.peek()).toEqual({});
		expect((await client.stats()).bytesReceived).toBe(42);
	} finally {
		client.close();
		server.close();
	}
});

test("Reload reports stats and probe only while connected", async () => {
	const original = globalThis.WebTransport;
	const pair = createMockTransportPair(Lite.ALPN_06_WIP, { stats: { bytesSent: 99 } });
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
		expect((await reload.stats())?.bytesSent).toBe(99);
		expect(reload.probe.peek()).toEqual({});

		server.close();
		while (reload.established.peek()) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
		expect(reload.status.peek()).toBe("disconnected");
		expect(await reload.stats()).toBeUndefined();
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(reload.probe.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});
