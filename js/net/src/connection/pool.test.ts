import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { Milli } from "@moq/clock";
import type { Producer as BroadcastProducer } from "../broadcast.ts";
import { SessionCode, SessionError } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import * as Time from "../time.ts";
import { connect } from "./connect.ts";
import { type AcceptProps, accept as acceptSession } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";
import { Reload } from "./reload.ts";

function publish(origin: { createBroadcast(path: Path.Valid): BroadcastProducer }, path: Path.Valid) {
	const broadcast = origin.createBroadcast(path);
	broadcast.announce();
	return broadcast;
}

const url = new URL("https://example.com/pool");

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
}

// Drain the mock handshake and reactive tasks before moving to the next timer slice.
async function advance(ms = 0) {
	for (let elapsed = 0; ; ) {
		for (let i = 0; i < 64; i++) await Promise.resolve();
		if (elapsed === ms) return;
		const step = Math.min(100, ms - elapsed);
		jest.advanceTimersByTime(step);
		elapsed += step;
	}
}

const linger = Milli(20);

const original = globalThis.WebTransport;
const originalWebSocket = globalThis.WebSocket;

beforeEach(() => jest.useFakeTimers());

afterEach(async () => {
	resetShared();
	// Evicted entries are outside the pool and retire on their last lease's linger timer.
	await advance(linger);
	globalThis.WebTransport = original;
	globalThis.WebSocket = originalWebSocket;
	jest.useRealTimers();
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

	await advance();
	expect(first.status.peek()).toBe("connected");
	expect(second.status.peek()).toBe("connected");
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(first.origin.peek());

	// One handle leaving doesn't disturb the other.
	first.close();
	await advance(linger);
	expect(second.status.peek()).toBe("connected");

	second.close();
});

test("the connection closes after the last handle and the linger window", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await advance();
	expect(handle.status.peek()).toBe("connected");
	const origin = handle.origin.peek();

	handle.close();
	// Idempotent: a double close must not double-release.
	handle.close();

	await advance(linger);
	expect(origin?.closed.peek()).not.toBeUndefined();

	// The next handle dials fresh.
	const next = new Connection({ url, linger });
	await advance();
	expect(next.status.peek()).toBe("connected");
	expect(dials.count()).toBe(2);
	next.close();
});

test("a handle taken within the linger window reuses the warm connection", async () => {
	const dials = stubTransports();

	const first = new Connection({ url, linger: Milli(10_000) });
	await advance();
	expect(first.status.peek()).toBe("connected");
	const origin = first.origin.peek();
	first.close();

	const second = new Connection({ url });
	await advance();
	expect(second.origin.peek()).toBeDefined();
	expect(dials.count()).toBe(1);
	expect(second.origin.peek()).toBe(origin);
	second.close();
});

test("the longest requested linger keeps the shared connection warm", async () => {
	stubTransports();

	const first = new Connection({ url, linger: Milli(1) });
	const second = new Connection({ url, linger: Milli(500) });
	await advance();
	expect(second.status.peek()).toBe("connected");
	const origin = first.origin.peek();

	first.close();
	second.close();
	await advance(499);

	expect(origin?.closed.peek()).toBeUndefined();
	await advance(1);
	expect(origin?.closed.peek()).not.toBeUndefined();
});

test("disabling a handle releases its share", async () => {
	stubTransports();

	const toggled = new Connection({ url, linger });
	const steady = new Connection({ url });
	await advance();
	expect(toggled.status.peek()).toBe("connected");

	toggled.enabled.set(false);
	await advance();
	expect(toggled.origin.peek()).toBeUndefined();
	expect(toggled.status.peek()).not.toBe("connected");

	// The steady handle keeps the connection alive through the toggle.
	await advance(linger);
	expect(steady.status.peek()).toBe("connected");

	// Re-enabling rejoins the shared connection.
	toggled.enabled.set(true);
	await advance();
	expect(toggled.status.peek()).toBe("connected");
	expect(toggled.origin.peek()).toBe(steady.origin.peek());

	toggled.close();
	steady.close();
});

test("switching URLs switches origins", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await advance();
	expect(handle.origin.peek()).toBeDefined();
	const before = handle.origin.peek();

	handle.url.set(new URL("https://example.com/other"));
	await advance();
	expect(handle.origin.peek()).toBeDefined();
	expect(handle.origin.peek()).not.toBe(before);
	expect(dials.count()).toBe(2);

	handle.close();
});

test("a shared connection outlasts an outage longer than a finite retry window", async () => {
	let offline = true;
	const stub = function StubWebTransport() {
		if (offline) throw new Error("relay is down");
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// Keep the fallback local too, so offline attempts are driven entirely by fake time.
	globalThis.WebSocket = function OfflineWebSocket() {
		throw new Error("relay is down");
	} as unknown as typeof WebSocket;

	const handle = new Connection({ url, linger });
	try {
		await advance(500);
		expect(handle.status.peek()).toBe("disconnected");

		// A pooled connection must recover even after a minute offline.
		await advance(60_000);
		expect(handle.status.peek()).not.toBe("connected");

		offline = false;
		await advance(10_000);
		expect(handle.status.peek()).toBe("connected");
	} finally {
		handle.close();
	}
});

test("a publish through one handle resolves locally for another", async () => {
	stubTransports();

	const publisher = new Connection({ url, linger });
	const watcher = new Connection({ url });
	await advance();
	expect(publisher.origin.peek()).toBeDefined();

	const origin = publisher.origin.peek();
	if (!origin) throw new Error("expected an origin");
	const broadcast = publish(origin, Path.from("mine"));
	broadcast.createTrack("chat", { timescale: Time.Timescale.MILLI });

	// Loopback: the shared origin serves the page's own publish with no round trip, so the
	// request resolves synchronously instead of waiting on the relay to announce it back.
	const request = watcher.origin.peek()?.request(Path.from("mine"));
	expect(request?.active.peek()).toBeDefined();
	request?.close();

	broadcast.close();
	publisher.close();
	watcher.close();
});

test("a borrowed connection origin exposes live routes and dynamic serving", async () => {
	stubTransports();
	const handle = new Connection({ url, linger });
	try {
		await advance();
		expect(handle.origin.peek()).toBeDefined();
		const table = handle.origin.peek();
		if (!table) throw new Error("expected an origin");
		const live = table.announced();
		let announcement: unknown;
		void live.next().then((event) => {
			announcement = event;
		});
		const prefix = Path.from("on-demand");
		const route = table.dynamic(prefix);
		await advance();
		expect(announcement).toMatchObject({ kind: "start", prefix });
		let withdrawal: unknown;
		void live.next().then((event) => {
			withdrawal = event;
		});
		route.close();
		await advance();
		expect(withdrawal).toMatchObject({ kind: "end", prefix });
		live.close();
	} finally {
		handle.close();
	}
});

test("share: false keeps a private loop and origin", async () => {
	const dials = stubTransports();

	const shared = new Connection({ url, linger });
	const privateLoop = new Connection({ url, linger, share: false });

	await advance();
	expect(shared.status.peek()).toBe("connected");
	expect(privateLoop.status.peek()).toBe("connected");
	expect(dials.count()).toBe(2);
	expect(privateLoop.origin.peek()).not.toBe(shared.origin.peek());

	shared.close();
	privateLoop.close();
});

test("connection-specific options infer a private loop and origin", async () => {
	const dials = stubTransports();
	const origin = new OriginProducer();
	const shared = new Connection({ url, linger });
	try {
		await advance();
		expect(shared.status.peek()).toBe("connected");
		const options = [
			{ webtransport: { congestionControl: "throughput" as const } },
			{ websocket: { enabled: false } },
			{ delay: { timeout: Milli(0) } },
			{ publish: origin.consume() },
			{ consume: origin },
		];
		for (const option of options) {
			const handle = new Connection({ url, ...option });
			try {
				await advance();
				expect(handle.status.peek()).toBe("connected");
				expect(handle.origin.peek()).not.toBe(shared.origin.peek());
			} finally {
				handle.close();
			}
		}
		expect(dials.count()).toBe(1 + options.length);
	} finally {
		shared.close();
		origin.close();
	}
});

test("explicit share: true refuses connection-specific options", () => {
	const origin = new OriginProducer();
	try {
		expect(() => new Connection({ share: true, consume: origin })).toThrow(/share: false/);
		expect(() => new Connection({ share: true, publish: origin.consume() })).toThrow(/share: false/);
		expect(() => new Connection({ share: true, webtransport: { serverCertificate: "x" } })).toThrow(/share: false/);
		expect(
			() => new Connection({ share: true, webtransport: { serverCertificateHashes: [{ value: "aa" }] } }),
		).toThrow(/share: false/);
		expect(() => new Connection({ share: true, webtransport: { congestionControl: "throughput" } })).toThrow(
			/share: false/,
		);
		expect(() => new Connection({ share: true, websocket: { enabled: false } })).toThrow(/share: false/);
		expect(() => new Connection({ share: true, delay: { timeout: Milli(0) } })).toThrow(/share: false/);
	} finally {
		origin.close();
	}
});

test("the removed discovery option is refused rather than ignored", async () => {
	const stale = { discovery: false } as never;
	expect(() => new Connection(stale)).toThrow(TypeError);
	expect(() => new Reload(stale)).toThrow(TypeError);
	await expect(connect({ url, ...(stale as object) })).rejects.toThrow(TypeError);
	const pair = createMockTransportPair(Lite.ALPN_05);
	await expect(acceptSession({ transport: pair.server, url, ...(stale as object) })).rejects.toThrow(TypeError);
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
			await advance();
			expect(handle.error.peek()).toBeDefined();
			expect(handle.error.peek()).toBeInstanceOf(SessionError);
			expect((handle.error.peek() as SessionError).code).toBe(SessionCode.Unauthorized);
			expect(handle.closed.peek()).toBeUndefined();

			handle.url.set(fresh);
			await advance();
			expect(handle.status.peek()).toBe("connected");
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
			await advance();
			expect(handle.error.peek()).toBeDefined();
			const givenUp = dials.count();
			expect(handle.closed.peek()).toBeUndefined();

			handle.enabled.set(false);
			await advance();
			expect(handle.origin.peek()).toBeUndefined();
			handle.enabled.set(true);
			await advance();
			expect(dials.count()).toBeGreaterThan(givenUp);
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
		delay: { initial: Milli(1), multiplier: 1, max: Milli(1), timeout: Milli(1) },
	});
	try {
		await advance(2);
		expect(handle.error.peek()).toBeDefined();
		expect(handle.closed.peek()).toBeUndefined();
		const givenUp = attempts;

		handle.url.set(new URL("https://example.com/other"));
		await advance();
		expect(attempts).toBeGreaterThan(givenUp);
		expect(handle.closed.peek()).toBeUndefined();
	} finally {
		handle.close();
	}
});

test("a private partial delay keeps the unlimited retry timeout", async () => {
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
		delay: { initial: Milli(1000), multiplier: 1, max: Milli(1000) },
	});
	try {
		await advance(60_000);
		expect(attempts).toBeGreaterThanOrEqual(3);
		expect(handle.error.peek()).toBeUndefined();
	} finally {
		handle.close();
	}
});

test("a private handle with an explicitly undefined timeout outlasts the outage", async () => {
	let offline = true;
	let attempts = 0;
	const stub = function StubWebTransport() {
		attempts++;
		if (offline) throw new Error("relay is down");
		const pair = createMockTransportPair(Lite.ALPN_05);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// What a caller building options from optional values produces: the undefined means "the
	// default", which for a Connection is to retry for as long as the handle is held.
	const handle = new Connection({ url, share: false, websocket: { enabled: false }, delay: { timeout: undefined } });
	try {
		// Long enough that any finite fallback window would have given up.
		await advance(60_000);
		expect(attempts).toBeGreaterThan(3);
		expect(handle.error.peek()).toBeUndefined();

		offline = false;
		await advance(10_000);
		expect(handle.status.peek()).toBe("connected");
	} finally {
		handle.close();
	}
});

test("a closed handle cannot reconnect", async () => {
	const dials = stubTransports();

	const handle = new Connection({ url, linger });
	await advance();
	expect(handle.status.peek()).toBe("connected");
	const abort = new Error("done");
	handle.close(abort);
	expect(handle.closed.peek()).toBe(abort);

	const before = dials.count();
	handle.url.set(new URL("https://example.com/other"));
	handle.enabled.set(false);
	handle.enabled.set(true);
	await advance();
	expect(dials.count()).toBe(before);
});

test("auth eviction lets a later handle dial fresh and the old lease still cleans up", async () => {
	const stale = new URL("https://example.com/pool?jwt=evict");
	const dials = stubUnauthorized(stale);

	const first = new Connection({ url: stale, linger });
	await advance();
	expect(first.error.peek()).toBeDefined();
	const origin = first.origin.peek();
	expect(origin).toBeDefined();
	expect(first.closed.peek()).toBeUndefined();

	const second = new Connection({ url: stale, linger });
	await advance();
	expect(dials.count()).toBeGreaterThanOrEqual(2);
	expect(second.origin.peek()).not.toBe(origin);

	first.close();
	await advance(linger);
	expect(origin?.closed.peek()).not.toBeUndefined();
	expect(second.closed.peek()).toBeUndefined();

	second.close();
});

test("pooled probe readers share demand and carry it through reconnects", async () => {
	const pairs: ReturnType<typeof createMockTransportPair>[] = [];
	const statsCalls: number[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06, { stats: { estimatedSendRate: 1_000_000 } });
		const index = pairs.length;
		statsCalls.push(0);
		const stats = pair.server.getStats.bind(pair.server);
		pair.server.getStats = () => {
			statsCalls[index]++;
			return stats();
		};
		pairs.push(pair);
		void accept(pair.server, url);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	const settle = () =>
		new Promise<void>((resolve) => {
			const channel = new MessageChannel();
			channel.port1.onmessage = () => {
				channel.port1.close();
				channel.port2.close();
				resolve();
			};
			channel.port2.postMessage(null);
		});
	const first = new Connection({ url, linger });
	const second = new Connection({ url, linger });
	let stopFirst = () => {};
	let stopSecond = () => {};
	try {
		await advance();
		await settle();
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(0);
		stopFirst = first.probe.subscribe(() => {});
		stopSecond = second.probe.subscribe(() => {});
		await advance();
		await settle();
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(1);
		stopFirst();
		await advance(100);
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(1);
		pairs[0].server.close({ closeCode: SessionCode.Internal, reason: "reconnect" });
		await advance(2_000);
		await settle();
		expect(pairs).toHaveLength(2);
		expect(pairs[1].client.sendStreams.bidi).toHaveLength(1);
		stopSecond();
		await advance();
		await settle();
		const before = statsCalls[1];
		await advance(500);
		expect(statsCalls[1]).toBe(before);
	} finally {
		stopFirst();
		stopSecond();
		first.close();
		second.close();
		await advance(linger);
		await settle();
	}
});
