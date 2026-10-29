import { afterEach, beforeEach, expect, test } from "bun:test";
import { type Installed, Manual, Milli, Nano } from "@moq/clock";
import { Signal } from "@moq/signals";
import { SessionCode, SessionError } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer, type Table } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import * as Time from "../time.ts";
import { connect } from "./connect.ts";
import { type AcceptProps, accept as acceptSession } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";
import { Reload } from "./reload.ts";

function publish(origin: Pick<Table, "createBroadcast">, path: Path.Valid) {
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	return broadcast;
}

const url = new URL("https://example.com/pool");

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
}

let clock: Manual;
let installed: Installed;

function advance(ms = 0): Promise<void> {
	return clock.advance(Nano.fromMilli(Milli(ms)));
}

const linger = Milli(2000);

const original = globalThis.WebTransport;
const originalWebSocket = globalThis.WebSocket;

beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});

afterEach(async () => {
	resetShared();
	// Evicted entries are outside the pool and retire on their last lease's linger timer.
	await advance(linger);
	globalThis.WebTransport = original;
	globalThis.WebSocket = originalWebSocket;
	installed.close();
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

	const first = new Connection({ url });
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

	const handle = new Connection({ url });
	await advance();
	expect(handle.status.peek()).toBe("connected");
	const origin = handle.origin.peek();

	handle.close();
	// Idempotent: a double close must not double-release.
	handle.close();

	await advance(linger);
	expect(origin?.closed.peek()).not.toBeUndefined();

	// The next handle dials fresh.
	const next = new Connection({ url });
	await advance();
	expect(next.status.peek()).toBe("connected");
	expect(dials.count()).toBe(2);
	next.close();
});

test("a handle taken within the linger window reuses the warm connection", async () => {
	const dials = stubTransports();

	const first = new Connection({ url });
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

test("rejoining cancels expiry and the last release starts a fresh two-second window", async () => {
	stubTransports();

	const first = new Connection({ url });
	await advance();
	const origin = first.origin.peek();
	first.close();
	await advance(1999);
	expect(origin?.closed.peek()).toBeUndefined();

	const second = new Connection({ url });
	await advance();
	expect(second.origin.peek()).toBe(origin);
	await advance(1);
	expect(origin?.closed.peek()).toBeUndefined();

	second.close();
	await advance(1999);
	expect(origin?.closed.peek()).toBeUndefined();
	await advance(1);
	expect(origin?.closed.peek()).not.toBeUndefined();
});

test("disabling a handle releases its share", async () => {
	stubTransports();

	const toggled = new Connection({ url });
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

	const handle = new Connection({ url });
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

	const handle = new Connection({ url });
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

	const publisher = new Connection({ url });
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
	const handle = new Connection({ url });
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
		const route = table.requestsRoute(prefix);
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

	const shared = new Connection({ url });
	const privateLoop = new Connection({ url, share: false });

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
	const shared = new Connection({ url });
	try {
		await advance();
		expect(shared.status.peek()).toBe("connected");
		const options = [
			{ webtransport: { congestionControl: "throughput" as const } },
			{ websocket: { enabled: false } },
			{ delay: { timeout: Milli(0) } },
			{ timeout: Milli(10_000) },
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
		expect(() => new Connection({ share: true, timeout: Milli(10_000) })).toThrow(/share: false/);
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
	const opts = share ? {} : { share: false as const, websocket: { enabled: false } };

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

	const handle = new Connection({ url });
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

	const first = new Connection({ url: stale });
	await advance();
	expect(first.error.peek()).toBeDefined();
	const origin = first.origin.peek();
	expect(origin).toBeDefined();
	expect(first.closed.peek()).toBeUndefined();

	const second = new Connection({ url: stale });
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
	const first = new Connection({ url });
	const second = new Connection({ url });
	let stopFirst = () => {};
	let stopSecond = () => {};
	try {
		await advance();
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(0);
		stopFirst = first.probe.subscribe(() => {});
		stopSecond = second.probe.subscribe(() => {});
		await advance();
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(1);
		stopFirst();
		await advance(100);
		expect(pairs[0].client.sendStreams.bidi).toHaveLength(1);
		pairs[0].server.close({ closeCode: SessionCode.Internal, reason: "reconnect" });
		await advance(2_000);
		expect(pairs).toHaveLength(2);
		expect(pairs[1].client.sendStreams.bidi).toHaveLength(1);
		stopSecond();
		await advance();
		const before = statsCalls[1];
		await advance(500);
		expect(statsCalls[1]).toBe(before);
	} finally {
		stopFirst();
		stopSecond();
		first.close();
		second.close();
		await advance(linger);
	}
});

test("a handle's announcements sync with the shared origin", async () => {
	stubTransports();

	const handle = new Connection({ url });
	const watch = handle.announced();
	expect(watch.synced.peek()).toBe(false);

	await advance();
	expect(handle.status.peek()).toBe("connected");
	// The session's announce request answers after it establishes.
	await advance();
	expect(watch.synced.peek()).toBe(true);

	watch.close();
	handle.close();
});

test("shared connections distinguish ordered versions", async () => {
	const dials = stubTransports();
	const versions = ["moq-lite-07-wip", "moq-lite-06", "moq-lite-05"];
	const first = new Connection({ url, versions, share: true });
	const same = new Connection({ url, versions: [...versions], share: true });
	const reversed = new Connection({ url, versions: [...versions].reverse(), share: true });
	const defaults = new Connection({ url });
	try {
		await advance();
		expect(first.status.peek()).toBe("connected");
		expect(same.origin.peek()).toBe(first.origin.peek());
		expect(reversed.origin.peek()).not.toBe(first.origin.peek());
		expect(defaults.origin.peek()).not.toBe(first.origin.peek());
		expect(dials.count()).toBe(3);
	} finally {
		first.close();
		same.close();
		reversed.close();
		defaults.close();
	}
});

test("a caller cannot mutate acquired versions", async () => {
	const dials = stubTransports();
	const versions = ["moq-lite-07-wip", "moq-lite-05"];
	const first = new Connection({ url, versions });
	versions.reverse();
	const original = new Connection({ url, versions: ["moq-lite-07-wip", "moq-lite-05"] });
	const changed = new Connection({ url, versions });
	try {
		await advance();
		expect(first.origin.peek()).toBe(original.origin.peek());
		expect(changed.origin.peek()).not.toBe(first.origin.peek());
		expect(dials.count()).toBe(2);
	} finally {
		first.close();
		original.close();
		changed.close();
	}
});

/** Answer each dial with the first ALPN it offers, recording every offer. */
function stubOffers(): string[][] {
	const offers: string[][] = [];
	globalThis.WebTransport = function OfferTransport(_address: string | URL, options?: WebTransportOptions) {
		const protocols = [...(options?.protocols ?? [])];
		offers.push(protocols);
		const pair = createMockTransportPair(protocols[0]);
		void accept(pair.server, url);
		return pair.client;
	} as unknown as typeof WebTransport;
	return offers;
}

for (const share of [true, false]) {
	test(`updating versions redials with the new list (share: ${share})`, async () => {
		const offers = stubOffers();
		const versions = new Signal<readonly string[] | undefined>(["moq-lite-05"]);
		const handle = new Connection({
			url,
			versions,
			share,
			websocket: share ? undefined : { enabled: false },
		});
		try {
			await advance();
			expect(handle.status.peek()).toBe("connected");
			const origin = handle.origin.peek();

			// An equal list is not a change.
			versions.set(["moq-lite-05"]);
			await advance();
			expect(offers).toEqual([[Lite.ALPN_05]]);

			versions.set(["moq-lite-06"]);
			await advance();
			expect(handle.status.peek()).toBe("connected");
			expect(offers).toEqual([[Lite.ALPN_05], [Lite.ALPN_06]]);
			// A shared handle moves to another pool entry; a private one keeps its own origin.
			if (share) expect(handle.origin.peek()).not.toBe(origin);
			else expect(handle.origin.peek()).toBe(origin);
		} finally {
			handle.close();
		}
	});
}

test("a live version list naming an unknown version fails on error without dialing", async () => {
	const offers = stubOffers();
	const versions = new Signal<readonly string[] | undefined>(["moq-lite-99"]);
	const handle = new Connection({ url, versions });
	try {
		await advance();
		expect(handle.error.peek()).toBeInstanceOf(TypeError);
		expect(handle.origin.peek()).toBeUndefined();
		expect(offers).toHaveLength(0);

		versions.set(["moq-lite-06"]);
		await advance();
		expect(handle.error.peek()).toBeUndefined();
		expect(handle.status.peek()).toBe("connected");
		expect(offers).toEqual([[Lite.ALPN_06]]);
	} finally {
		handle.close();
	}
});

test("GOAWAY keeps version pools distinct at the redirected URL", async () => {
	const moved = new URL("https://example.com:5443/pool");
	const peers: WebTransport[] = [];
	globalThis.WebTransport = function RedirectTransport(address: string | URL) {
		const pair = createMockTransportPair(Lite.ALPN_06);
		peers.push(pair.server);
		void accept(pair.server, new URL(address));
		return pair.client;
	} as unknown as typeof WebTransport;
	const versions = ["moq-lite-07-wip", "moq-lite-06"];
	const first = new Connection({ url, versions });
	const resident = new Connection({ url: moved });
	let joined: Connection | undefined;
	let defaults: Connection | undefined;
	try {
		await advance();
		const origin = first.origin.peek();
		// Exercise a healthy migration after the immediate-redirect backoff guard.
		await advance(2_000);
		const peer = peers[0];
		if (!peer) throw new Error("missing initial peer");
		const stream = await Stream.open(peer, { version: Lite.Version.DRAFT_06 });
		await stream.writer.u53(Lite.StreamId.Goaway);
		await new Lite.Goaway(moved.href).encode(stream.writer, Lite.Version.DRAFT_06);
		await advance();
		joined = new Connection({ url: moved, versions });
		defaults = new Connection({ url: moved });
		await advance();
		expect(joined.status.peek()).toBe("connected");
		expect(joined.origin.peek()).toBe(origin);
		expect(defaults.origin.peek()).toBe(resident.origin.peek());
		expect(defaults.origin.peek()).not.toBe(origin);
		expect(peers).toHaveLength(3);
	} finally {
		first.close();
		resident.close();
		joined?.close();
		defaults?.close();
	}
});

test("a private handle's timeout bounds each attempt", async () => {
	let attempts = 0;
	class Hung {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});
		close() {}
	}
	globalThis.WebTransport = function StubWebTransport() {
		attempts++;
		return new Hung();
	} as unknown as typeof WebTransport;

	const handle = new Connection({
		url,
		share: false,
		websocket: { enabled: false },
		timeout: Milli(50),
		delay: { initial: Milli(10), multiplier: 1, max: Milli(10) },
	});
	try {
		// Each hung dial is cut at 50 ms and redialed after the backoff; the default 30 s
		// timeout would still be holding the first.
		await advance(1_000);
		expect(attempts).toBeGreaterThanOrEqual(3);
	} finally {
		handle.close();
	}
});

test("an invalid timeout is refused when a private handle is built", () => {
	expect(() => new Connection({ url, share: false, timeout: Number.NaN as Milli })).toThrow(RangeError);
});
