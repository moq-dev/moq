import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { accept } from "./index.ts";
import { Reload, type ReloadProps } from "./reload.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

test("equivalent URL instances do not restart a pending connection", async () => {
	const original = globalThis.WebTransport;
	let connects = 0;

	class PendingWebTransport {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});

		constructor() {
			connects++;
		}

		close() {}
	}

	globalThis.WebTransport = PendingWebTransport as unknown as typeof WebTransport;
	const reload = new Reload({
		enabled: true,
		url: new URL("https://example.com/broadcast"),
		websocket: { enabled: false },
	});

	try {
		await settle();
		expect(connects).toBe(1);

		reload.url.set(new URL("https://example.com/broadcast"));
		await settle();
		expect(connects).toBe(1);

		reload.url.set(new URL("https://example.com/other"));
		await settle();
		expect(connects).toBe(2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("ReloadProps excludes signal", () => {
	// @ts-expect-error signal is not part of ReloadProps
	const props: ReloadProps = { signal: new AbortController().signal };
	expect(props.enabled).toBeUndefined();
});

test("closing mid-connect aborts the pending attempt", async () => {
	const original = globalThis.WebTransport;
	let closes = 0;

	class PendingWebTransport {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});

		close() {
			closes++;
		}
	}

	globalThis.WebTransport = PendingWebTransport as unknown as typeof WebTransport;
	const reload = new Reload({
		enabled: true,
		url: new URL("https://example.com/broadcast"),
		websocket: { enabled: false },
	});

	try {
		await settle();
		expect(closes).toBe(0);

		reload.close();
		await settle();
		expect(closes).toBe(1);
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

test("a failure no retry can clear stops after one attempt", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	let connects = 0;

	// The relay answers with an ALPN this build doesn't speak, which `connect` reports as
	// `Terminal`. Redialing produces the same answer forever, so the loop has to stop.
	const stub = function StubWebTransport() {
		connects++;
		return createMockTransportPair("moq-from-the-future").client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// A delay far longer than the test's patience: reaching the rejection at all proves nothing
	// was scheduled, and the count proves it wasn't retried.
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: 60000, multiplier: 2, max: 60000 },
	});

	try {
		await expect(reload.closed).rejects.toThrow(/unsupported WebTransport protocol/);
		expect(connects).toBe(1);
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
