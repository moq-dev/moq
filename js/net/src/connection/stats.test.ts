import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { Effect } from "@moq/signals";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { type AcceptProps, accept as acceptSession, type ConnectProps, connect as connectSession } from "./index.ts";
import { Reload } from "./reload.ts";
import { type TransportStats, transportStats } from "./stats.ts";

function connect(url: URL, props: Omit<ConnectProps, "url"> = {}) {
	return connectSession({ url, ...props });
}

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
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
	expect(stats.rtt).toBe(Milli(25));
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
	const pair = createMockTransportPair(Lite.ALPN_06, {
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
		client.abort();
		server.abort();
	}
});

test("stats() is empty on a transport without getStats", async () => {
	const pair = createMockTransportPair(Lite.ALPN_06);
	// Shadow the mock's getStats to imitate the qmux/WebSocket fallback.
	(pair.client as unknown as { getStats?: unknown }).getStats = undefined;
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	try {
		expect(await client.stats()).toEqual({});
	} finally {
		client.abort();
		server.abort();
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
		client.abort();
		server.abort();
	}
});

test("Reload exposes its established session stats and probe only while connected", async () => {
	const original = globalThis.WebTransport;
	const pair = createMockTransportPair(Lite.ALPN_06, { stats: { bytesSent: 99 } });
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
		expect((await reload.established.peek()?.stats())?.bytesSent).toBe(99);
		expect(reload.probe.peek()).toEqual({});

		server.abort();
		while (reload.established.peek()) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
		expect(reload.status.peek()).toBe("disconnected");
		expect(await reload.established.peek()?.stats()).toBeUndefined();
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(reload.probe.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a lite connection probes only while its estimate is observed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const drain = () => clock.advance(Nano.zero);
	const pair = createMockTransportPair(Lite.ALPN_06, { stats: { estimatedSendRate: 1_000_000 } });
	const open = pair.client.createBidirectionalStream.bind(pair.client);
	let finished = 0;
	const opening = spyOn(pair.client, "createBidirectionalStream").mockImplementation(async (options) => {
		const stream = await open(options);
		const writer = stream.writable.getWriter();
		return {
			readable: stream.readable,
			writable: new WritableStream<Uint8Array>({
				write: (chunk) => writer.write(chunk),
				close: () => {
					finished++;
					return writer.close();
				},
				abort: (reason) => writer.abort(reason),
			}),
		};
	});
	const url = new URL("https://example.com/");
	const [client, server] = await Promise.all([connect(url, { transport: pair.client }), accept(pair.server, url)]);
	let stop = () => {};
	let effect: Effect | undefined;
	try {
		await drain();
		expect(client.probe.peek()).toEqual({});
		expect(pair.client.sendStreams.bidi).toHaveLength(0);
		stop = client.probe.subscribe(() => {});
		await drain();
		expect(pair.client.sendStreams.bidi).toHaveLength(1);
		const values: unknown[] = [];
		effect = new Effect((effect) => {
			values.push(effect.get(client.probe));
		});
		await drain();
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(values.at(-1)).toMatchObject({ estimatedRecvRate: 1_000_000 });
		// A reactive one-shot listener reattaches without dropping demand between updates.
		expect(pair.client.sendStreams.bidi).toHaveLength(1);
		stop();
		await drain();
		expect(finished).toBe(0);
		effect.close();
		await drain();
		expect(finished).toBe(1);
		expect(client.probe.peek()).toEqual({});
		stop = client.probe.subscribe(() => {});
		await drain();
		expect(pair.client.sendStreams.bidi).toHaveLength(2);
	} finally {
		stop();
		effect?.close();
		await drain();
		client.abort();
		server.abort();
		opening.mockRestore();
		installed.close();
	}
});
