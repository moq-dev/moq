import { afterEach, expect, test } from "bun:test";
import type { Producer as BroadcastProducer } from "../broadcast.ts";
import { SessionCode, SessionError } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { accept } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";

function publish(origin: { createBroadcast(path: Path.Valid): BroadcastProducer }, path: Path.Valid) {
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	return broadcast;
}

const url = new URL("https://example.com/pool");

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean, ms = 1000): Promise<void> {
	// Date, not performance: the retry test accelerates the latter.
	const deadline = Date.now() + ms;
	for (;;) {
		if (pred()) return;
		if (Date.now() > deadline) throw new Error("timed out waiting for condition");
		await settle();
	}
}

// A tiny window keeps the linger tests quick without mocking timers. The wait is a wide
// multiple of it so a loaded runner's timer drift can't be mistaken for lingering.
const linger = 20;

async function expired() {
	await new Promise((resolve) => setTimeout(resolve, linger * 15));
}

const original = globalThis.WebTransport;

afterEach(() => {
	resetShared();
	globalThis.WebTransport = original;
});

/** Hand out a live mock transport per dial, counting how many were opened. */
function stubTransports(): { count: () => number } {
	let count = 0;
	const stub = function StubWebTransport() {
		count++;
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	return { count: () => count };
}

test("two handles on one URL share a connection and an origin", async () => {
	const dials = stubTransports();

	const first = new Connection({ url, linger });
	const second = new Connection({ url });

	await waitUntil(() => first.status.peek() === "connected");
	await waitUntil(() => second.status.peek() === "connected");
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(first.origin.peek());

	// One handle leaving doesn't disturb the other.
	first.close();
	await expired();
	expect(second.status.peek()).toBe("connected");

	second.close();
});

test("the connection closes after the last handle and the linger window", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await waitUntil(() => handle.status.peek() === "connected");
	const origin = handle.origin.peek();

	handle.close();
	// Idempotent: a double close must not double-release.
	handle.close();

	await expired();
	expect(origin?.closed.peek()).not.toBeUndefined();

	// The next handle dials fresh.
	const next = new Connection({ url, linger });
	await waitUntil(() => next.status.peek() === "connected");
	expect(dials.count()).toBe(2);
	next.close();
});

test("a handle taken within the linger window reuses the warm connection", async () => {
	const dials = stubTransports();

	const first = new Connection({ url, linger: 10_000 });
	await waitUntil(() => first.status.peek() === "connected");
	const origin = first.origin.peek();
	first.close();

	const second = new Connection({ url });
	await waitUntil(() => second.origin.peek() !== undefined);
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(origin);
	second.close();
});

test("disabling a handle releases its share", async () => {
	stubTransports();

	const toggled = new Connection({ url, linger });
	const steady = new Connection({ url });
	await waitUntil(() => toggled.status.peek() === "connected");

	toggled.enabled.set(false);
	await waitUntil(() => toggled.origin.peek() === undefined);
	expect(toggled.status.peek()).not.toBe("connected");

	// The steady handle keeps the connection alive through the toggle.
	await expired();
	expect(steady.status.peek()).toBe("connected");

	// Re-enabling rejoins the shared connection.
	toggled.enabled.set(true);
	await waitUntil(() => toggled.status.peek() === "connected");
	expect(toggled.origin.peek()).toBe(steady.origin.peek());

	toggled.close();
	steady.close();
});

test("switching URLs switches origins", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await waitUntil(() => handle.origin.peek() !== undefined);
	const before = handle.origin.peek();

	handle.url.set(new URL("https://example.com/other"));
	await waitUntil(() => handle.origin.peek() !== undefined && handle.origin.peek() !== before);
	expect(dials.count()).toBe(2);

	handle.close();
});

test("a shared connection outlasts an outage longer than the default retry window", async () => {
	let offline = true;
	const stub = function StubWebTransport() {
		if (offline) throw new Error("relay is down");
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// The loop measures its retry window with performance.now, so speeding that clock up makes
	// a couple of real backoff waits look like minutes to it. A pooled connection has nobody
	// watching `closed` to redial it, so giving up would leave every handle on this URL dark
	// until the page reloads, however long the relay has been back.
	const real = performance.now.bind(performance);
	const start = real();
	performance.now = () => start + (real() - start) * 1000;

	try {
		const handle = new Connection({ url, linger });
		await waitUntil(() => handle.status.peek() === "disconnected");

		// Two backoff waits, which is many times over the default window on this clock.
		await new Promise((resolve) => setTimeout(resolve, 2000));
		expect(handle.status.peek()).not.toBe("connected");

		offline = false;
		await waitUntil(() => handle.status.peek() === "connected", 15_000);

		handle.close();
	} finally {
		performance.now = real;
	}
}, 30_000);

test("a publish through one handle resolves locally for another", async () => {
	stubTransports();

	const publisher = new Connection({ url, linger });
	const watcher = new Connection({ url });
	await waitUntil(() => publisher.origin.peek() !== undefined);

	const origin = publisher.origin.peek();
	if (!origin) throw new Error("expected an origin");
	const broadcast = publish(origin, Path.from("mine"));
	broadcast.createTrack("chat");

	// Loopback: the shared origin serves the page's own publish with no round trip, so the
	// request resolves synchronously instead of waiting on the relay to announce it back.
	const request = watcher.origin.peek()?.request(Path.from("mine"));
	expect(request?.active.peek()).toBeDefined();
	request?.close();

	broadcast.close();
	publisher.close();
	watcher.close();
});

test("share: false keeps a private loop and origin", async () => {
	const dials = stubTransports();

	const shared = new Connection({ url, linger });
	const privateLoop = new Connection({ url, linger, share: false });

	await waitUntil(() => shared.status.peek() === "connected");
	await waitUntil(() => privateLoop.status.peek() === "connected");
	expect(dials.count()).toBe(2);
	expect(privateLoop.origin.peek()).not.toBe(shared.origin.peek());

	shared.close();
	privateLoop.close();
});

test("caller-owned origins, transport options, and delay refuse to share", () => {
	const origin = new OriginProducer();
	try {
		expect(() => new Connection({ subscribe: origin })).toThrow(/share: false/);
		expect(() => new Connection({ publish: origin.consume() })).toThrow(/share: false/);
		expect(() => new Connection({ webtransport: { serverCertificate: "x" } })).toThrow(/share: false/);
		expect(() => new Connection({ webtransport: { serverCertificateHashes: [{ value: "aa" }] } })).toThrow(
			/share: false/,
		);
		expect(() => new Connection({ webtransport: { congestionControl: "throughput" } })).toThrow(/share: false/);
		expect(() => new Connection({ websocket: { enabled: false } })).toThrow(/share: false/);
		expect(() => new Connection({ discovery: false })).toThrow(/share: false/);
		expect(() => new Connection({ delay: { timeout: 0 } })).toThrow(/share: false/);
	} finally {
		origin.close();
	}
});

test("a supplied transport cannot enter the reconnect loop", () => {
	expect(() => new Connection({ transport: {} as never })).toThrow(/Connection\.connect/);
});

/** Reject the given URL as unauthorized; any other URL is accepted. */
function stubUnauthorized(stale: URL): { count: () => number } {
	let count = 0;
	const stub = function StubWebTransport(url: string | URL) {
		count++;
		const target = new URL(String(url));
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, target).then(() => {
			if (target.href === stale.href) {
				pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
			}
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	return { count: () => count };
}

for (const share of [true, false] as const) {
	const opts = share ? { linger } : { linger, share: false as const, websocket: { enabled: false } };

	test(`auth failure then a new URL recovers a ${share ? "shared" : "private"} handle`, async () => {
		const stale = new URL("https://example.com/pool?jwt=stale");
		const fresh = new URL("https://example.com/pool?jwt=fresh");
		const dials = stubUnauthorized(stale);

		const handle = new Connection({ url: stale, ...opts });
		try {
			await waitUntil(() => handle.error.peek() !== undefined);
			expect(handle.error.peek()).toBeInstanceOf(SessionError);
			expect((handle.error.peek() as SessionError).code).toBe(SessionCode.Unauthorized);
			expect(handle.closed.peek()).toBeUndefined();

			handle.url.set(fresh);
			await waitUntil(() => handle.status.peek() === "connected");
			expect(handle.error.peek()).toBeUndefined();
			expect(handle.closed.peek()).toBeUndefined();
			expect(dials.count()).toBeGreaterThanOrEqual(2);
		} finally {
			handle.close();
			expect(handle.closed.peek()).toBeNull();
		}
	});

	test(`disable/re-enable after auth retries a ${share ? "shared" : "private"} handle`, async () => {
		const stale = new URL(`https://example.com/pool?jwt=stale-${share}`);
		const dials = stubUnauthorized(stale);

		const handle = new Connection({ url: stale, ...opts });
		try {
			await waitUntil(() => handle.error.peek() !== undefined);
			const givenUp = dials.count();
			expect(handle.closed.peek()).toBeUndefined();

			handle.enabled.set(false);
			await waitUntil(() => handle.origin.peek() === undefined);
			handle.enabled.set(true);
			await waitUntil(() => dials.count() > givenUp);
			expect(handle.closed.peek()).toBeUndefined();
		} finally {
			handle.close();
		}
	});
}

test("exhausted retries then a new URL recovers a private handle", async () => {
	let attempts = 0;
	const stub = function StubWebTransport() {
		attempts++;
		throw new Error("relay is down");
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const handle = new Connection({
		url,
		share: false,
		websocket: { enabled: false },
		delay: { initial: 1, multiplier: 1, max: 1, timeout: 1 },
	});
	try {
		await waitUntil(() => handle.error.peek() !== undefined);
		expect(handle.closed.peek()).toBeUndefined();
		const givenUp = attempts;

		handle.url.set(new URL("https://example.com/other"));
		await waitUntil(() => attempts > givenUp);
		expect(handle.closed.peek()).toBeUndefined();
	} finally {
		handle.close();
	}
});

test("a closed handle cannot reconnect", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await waitUntil(() => handle.status.peek() === "connected");
	const abort = new Error("done");
	handle.close(abort);
	expect(handle.closed.peek()).toBe(abort);

	const before = dials.count();
	handle.url.set(new URL("https://example.com/other"));
	handle.enabled.set(false);
	handle.enabled.set(true);
	await settle();
	expect(dials.count()).toBe(before);
});

test("auth eviction lets a later handle dial fresh and the old lease still cleans up", async () => {
	const stale = new URL("https://example.com/pool?jwt=evict");
	const dials = stubUnauthorized(stale);

	const first = new Connection({ url: stale, linger });
	await waitUntil(() => first.error.peek() !== undefined);
	const origin = first.origin.peek();
	expect(origin).toBeDefined();
	expect(first.closed.peek()).toBeUndefined();

	const second = new Connection({ url: stale, linger });
	await waitUntil(() => dials.count() >= 2);
	expect(second.origin.peek()).not.toBe(origin);

	first.close();
	await expired();
	expect(origin?.closed.peek()).not.toBeUndefined();
	expect(second.closed.peek()).toBeUndefined();

	second.close();
});
