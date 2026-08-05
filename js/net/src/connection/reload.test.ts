import { beforeEach, expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, createPendingTransports } from "../mock.ts";
import * as Path from "../path.ts";
import { accept } from "./index.ts";
import { resetPool } from "./pool.ts";
import { Reload } from "./reload.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

// Sessions are shared by URL, so a leftover one would answer the next case.
beforeEach(() => {
	resetPool();
});

test("equivalent URL instances do not restart a pending connection", async () => {
	const original = globalThis.WebTransport;
	const pending = createPendingTransports();
	globalThis.WebTransport = pending.transport;
	const reload = new Reload({
		enabled: true,
		url: new URL("https://example.com/broadcast"),
		websocket: { enabled: false },
	});

	try {
		await settle();
		expect(pending.connects()).toBe(1);

		reload.url.set(new URL("https://example.com/broadcast"));
		await settle();
		expect(pending.connects()).toBe(1);

		reload.url.set(new URL("https://example.com/other"));
		await settle();
		expect(pending.connects()).toBe(2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("aborting the signal stops the loop", async () => {
	const original = globalThis.WebTransport;
	const pending = createPendingTransports();
	globalThis.WebTransport = pending.transport;
	const controller = new AbortController();
	const reload = new Reload({
		url: new URL("https://example.com/signal"),
		websocket: { enabled: false },
		signal: controller.signal,
	});

	try {
		await settle();
		expect(reload.status.peek()).toBe("connecting");

		controller.abort();
		await settle();
		expect(pending.closes()).toBe(1);
		await reload.closed;
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("connecting is the default", async () => {
	const original = globalThis.WebTransport;
	const pending = createPendingTransports();
	globalThis.WebTransport = pending.transport;
	const reload = new Reload({ url: new URL("https://example.com/default"), websocket: { enabled: false } });

	try {
		await settle();
		expect(pending.connects()).toBe(1);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("two connections to one URL share a session", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/shared");
	let connects = 0;

	const stub = function StubWebTransport() {
		connects++;
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const first = new Reload({ url, websocket: { enabled: false } });
	const second = new Reload({ url, websocket: { enabled: false } });

	try {
		await waitUntil(() => first.established.peek() !== undefined && second.established.peek() !== undefined);
		expect(connects).toBe(1);

		// One going away leaves the other connected: the session belongs to both.
		first.close();
		await settle();
		expect(second.established.peek()).not.toBeUndefined();
		expect(second.status.peek()).toBe("connected");
	} finally {
		first.close();
		second.close();
		globalThis.WebTransport = original;
	}
});

test("reload: false gives up after one session", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/once");
	let connects = 0;

	const sessions: { close: () => void }[] = [];
	const stub = function StubWebTransport() {
		connects++;
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		void accept(pair.server, url).then((server) => sessions.push(server));
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({ url, websocket: { enabled: false }, reload: false });

	try {
		await waitUntil(() => reload.established.peek() !== undefined && sessions.length > 0);

		// A clean drop is the end of the line rather than the start of a backoff.
		sessions[0]?.close();
		await reload.closed;
		expect(connects).toBe(1);
		expect(reload.established.peek()).toBeUndefined();
		expect(reload.status.peek()).toBe("disconnected");

		// Nothing is scheduled, so it stays down.
		await settle();
		expect(connects).toBe(1);

		// Settling `closed` also tears the effect scope down, so the page listeners, the probe
		// computed, and the announce pumps go with it. A URL change proves it: a live scope
		// would rerun the connect effect and dial again.
		reload.url.set(new URL("https://example.com/once-again"));
		await settle();
		expect(connects).toBe(1);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("closing mid-connect aborts the pending attempt", async () => {
	const original = globalThis.WebTransport;
	const pending = createPendingTransports();
	globalThis.WebTransport = pending.transport;
	const reload = new Reload({
		enabled: true,
		url: new URL("https://example.com/broadcast"),
		websocket: { enabled: false },
	});

	try {
		await settle();
		expect(pending.closes()).toBe(0);

		reload.close();
		await settle();
		expect(pending.closes()).toBe(1);
	} finally {
		globalThis.WebTransport = original;
	}
});

test("a peer that severs immediately keeps escalating the backoff", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		// Sever the session as soon as the server side finishes the handshake.
		void accept(pair.server, url).then((server) => server.close());
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// Every session dies well within `initial`, so the backoff has to keep escalating
	// and the retry window has to expire. Resetting either on each successful connect
	// reconnects forever at the initial delay and never gives up.
	//
	// `initial` sits far above the in-process handshake so a loaded runner can't make a
	// session look healthy, and the tiny timeout gives up after one backoff.
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: 1000, multiplier: 2, max: 1000, timeout: 1 },
	});
	try {
		await expect(reload.closed).rejects.toThrow();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean): Promise<void> {
	for (let i = 0; i < 500; i++) {
		if (pred()) return;
		await settle();
	}
	throw new Error("timed out waiting for condition");
}

test("announcedBroadcast follows the reconnect loop", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");

	// Every connect attempt gets a fresh session, whose server publishes the path once the
	// handshake finishes. The client therefore always asks before the broadcast exists.
	const sessions: { close: () => void }[] = [];
	const published: BroadcastProducer[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		void accept(pair.server, url).then((server) => {
			sessions.push(server);
			const broadcast = new BroadcastProducer();
			published.push(broadcast);
			server.publish(Path.from("late"), broadcast);
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: 10, multiplier: 1, max: 10 },
	});
	const watched = reload.announcedBroadcast(Path.from("late"));

	try {
		await waitUntil(() => watched.active.peek() !== undefined);
		const first = watched.active.peek();

		// The session dies: the handle drops the broadcast rather than clinging to a dead one.
		sessions[0]?.close();
		await waitUntil(() => watched.active.peek() === undefined);

		// The reconnect re-announces it, and the handle re-consumes on the new session.
		await waitUntil(() => watched.active.peek() !== undefined);
		expect(watched.active.peek()).not.toBe(first);
	} finally {
		watched.close();
		reload.close();
		for (const broadcast of published) broadcast.close();
		for (const session of sessions) session.close();
		globalThis.WebTransport = original;
	}
});
