import { expect, spyOn, test } from "bun:test";
import { Instant, Manual, Milli, Nano, now, sleep, yieldNow } from "@moq/clock";
import { Effect, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import type { Producer as BroadcastProducer } from "../broadcast.ts";
import { ConnectTimeout, SessionCode, SessionError, StreamCode, toTransport } from "../error.ts";
import { Route } from "../hop.ts";
import { ALPN as IetfALPN } from "../ietf/version.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, type MockTransport, textFrame } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { Timescale } from "../time.ts";
import { closeUnwanted } from "../util/announce.ts";
import { wireOf } from "../wire.ts";
import type { Established } from "./established.ts";
import { type AcceptProps, accept as acceptSession } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";
import { pumpAnnounced, Redirects, Reload, type ReloadDelay, type ReloadProps } from "./reload.ts";

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
}

function publish(origin: OriginProducer, path: Path.Valid) {
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	return broadcast;
}

async function settle() {
	await yieldNow();
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
	const withSignal = { signal: new AbortController().signal };
	// @ts-expect-error signal is not part of ReloadProps
	const props: ReloadProps = withSignal;
	expect(props.enabled).toBeUndefined();
});

test("ReloadProps excludes transport", () => {
	const withTransport = { transport: {} as WebTransport };
	// @ts-expect-error transport is not part of ReloadProps
	const props: ReloadProps = withTransport;
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
	let attempts = 0;
	const stub = function StubWebTransport() {
		attempts++;
		const pair = createMockTransportPair(Lite.ALPN_06);
		// Sever the session as soon as the server side finishes the handshake.
		void accept(pair.server, url).then((server) => server.abort());
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
		delay: { initial: Milli(1000), multiplier: 2, max: Milli(1000), timeout: Milli(1) },
	});
	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		expect(reload.closed.peek()).toBeUndefined();

		// Giving up this URL does not dispose the loop: a disable/re-enable starts another sequence.
		const givenUp = attempts;
		await settle();
		expect(attempts).toBe(givenUp);

		reload.enabled.set(false);
		await settle();
		reload.enabled.set(true);
		await settle();
		expect(attempts).toBeGreaterThan(givenUp);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("an explicitly undefined delay field falls back to its default", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	let dials = 0;
	const stub = function StubWebTransport() {
		dials += 1;
		const pair = createMockTransportPair(Lite.ALPN_06);
		void accept(pair.server, url).then((server) => server.abort());
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	// What a caller building options from optional values produces. Spreading this over the
	// defaults would take the undefined as the answer, and a NaN backoff redials as fast as
	// the event loop allows.
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: undefined, multiplier: undefined, max: undefined, timeout: Milli(0) },
	});
	try {
		await waitUntil(() => dials > 0);
		await sleep(Nano.fromMilli(Milli(100)));
		// The default initial delay is 1000ms, so the first retry is still pending.
		expect(dials).toBe(1);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean): Promise<void> {
	const deadline = Instant.add(now(), Nano.fromMilli(Milli(1000)));
	while (now() <= deadline) {
		if (pred()) return;
		await settle();
	}
	throw new Error("timed out waiting for condition");
}

test("a follower follows the reconnect on the same handle", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");

	// Every connect attempt gets a fresh session, whose server publishes the path once the
	// handshake finishes. The client therefore always asks before the broadcast exists.
	const sessions: { abort: () => void }[] = [];
	const published: BroadcastProducer[] = [];
	const clientOrigin = new OriginProducer();
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06);
		const origin = new OriginProducer();
		void accept(pair.server, url, { publish: origin.consume() }).then((server) => {
			sessions.push(server);
			published.push(publish(origin, Path.from("late")));
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Milli(10), multiplier: 1, max: Milli(10) },
		consume: clientOrigin,
	});
	const watched = clientOrigin.follow(Path.from("late"));

	try {
		await waitUntil(() => watched.active.peek() !== undefined);
		const first = watched.active.peek();

		// The session dies; the same request follows the next session's announcement.
		sessions[0]?.abort();
		await waitUntil(() => watched.active.peek() !== undefined && watched.active.peek() !== first);
		expect(watched.closed.peek()).toBeUndefined();
	} finally {
		watched.close();
		reload.close();
		clientOrigin.close();
		for (const broadcast of published) broadcast.close();
		for (const session of sessions) session.abort();
		globalThis.WebTransport = original;
	}
});

test("an ended auth handle gives up instead of retrying", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	const servers: Promise<{ abort(): void }>[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06);
		servers.push(accept(pair.server, url));
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;
	const clock = new Manual();
	const installed = clock.install();
	// Bounded by task turns rather than wall time, advancing the clock a millisecond per turn
	// so a reconnect, if the loop tried one, would be dialed well within the bound.
	const until = async (done: () => boolean) => {
		for (let i = 0; i < 100 && !done(); i++) {
			await yieldNow();
			await clock.advance(Nano.fromMilli(Milli(1)));
		}
		expect(done()).toBe(true);
	};

	// An unbounded retry window, so only the ended handle can stop the loop.
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Milli(1), multiplier: 2, max: Milli(1), timeout: Milli(0) },
	});
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	try {
		await until(() => reload.established.peek() !== undefined);
		const aborted = new Error("auth handle aborted");
		reload.auth.abort(aborted);
		(await servers[0])?.abort();

		await until(() => reload.error.peek() !== undefined);
		expect(reload.error.peek()).toBe(aborted);
		expect(servers.length).toBe(1);
	} finally {
		warned.mockRestore();
		reload.close();
		installed.close();
		globalThis.WebTransport = original;
	}
});

test("a page hide after give-up does not retry the refused URL", async () => {
	const original = globalThis.WebTransport;
	const previousWindow = globalThis.window;
	const previousDocument = globalThis.document;
	const win = new EventTarget();
	const doc = Object.assign(new EventTarget(), { hidden: false });
	Object.assign(globalThis, { window: win, document: doc });

	const url = new URL("https://example.com/");
	let attempts = 0;
	const stub = function StubWebTransport() {
		attempts++;
		const pair = createMockTransportPair(Lite.ALPN_06);
		void accept(pair.server, url).then(() => {
			pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Milli(1), multiplier: 1, max: Milli(1), timeout: Milli(0) },
	});

	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		const givenUp = attempts;
		const err = reload.error.peek();

		// Suspension shares sequence reset with give-up; resume must not look like a new
		// sequence or it would clear `error` and redial the JWT the peer already refused.
		win.dispatchEvent(new Event("pagehide"));
		await settle();
		win.dispatchEvent(new Event("pageshow"));
		for (let i = 0; i < 20; i++) await settle();
		expect(attempts).toBe(givenUp);
		expect(reload.error.peek()).toBe(err);
		expect(reload.closed.peek()).toBeUndefined();

		reload.url.set(new URL("https://example.com/changed"));
		await waitUntil(() => attempts > givenUp);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
		Object.assign(globalThis, { window: previousWindow, document: previousDocument });
	}
});

for (const code of [SessionCode.Unauthorized, SessionCode.MalformedAuthToken, SessionCode.Expired]) {
	test(`a session rejected with ${code} surfaces the code and stops retrying`, async () => {
		const original = globalThis.WebTransport;
		const url = new URL("https://example.com/");
		const closes: (Error | null)[] = [];
		const stub = function StubWebTransport() {
			const pair = createMockTransportPair(Lite.ALPN_06);
			// Reject at the MoQ layer: accept the transport, then close with a code, the
			// way a relay's Request::close does after it has already accepted the transport.
			void accept(pair.server, url).then(() => {
				pair.server.close({ closeCode: code, reason: "unauthorized" });
			});
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;

		const reload = new Reload({
			enabled: true,
			url,
			websocket: { enabled: false },
			delay: { initial: Milli(1), multiplier: 2, max: Milli(1), timeout: Milli(0) },
		});
		const watch = new Effect();
		watch.run((effect) => {
			const conn = effect.get(reload.established);
			if (conn) {
				effect.spawn(async () => {
					closes.push(await conn.closed);
				});
			}
		});

		try {
			// The code reaches the app rather than being flattened away, and because
			// the code is specified rather than guessed at, the loop stops this URL
			// instead of retrying credentials that cannot work. `timeout: 0` means unlimited
			// retries, so `error` settling at all is what proves it stopped on the rejection.
			await waitUntil(() => reload.error.peek() !== undefined);
			const err = reload.error.peek();
			expect(err).toBeInstanceOf(SessionError);
			expect((err as SessionError).code).toBe(code);
			expect(reload.closed.peek()).toBeUndefined();

			// It still surfaced through the established session before the loop gave up.
			expect(closes.find((e) => e instanceof SessionError)).toBeInstanceOf(SessionError);
		} finally {
			watch.close();
			reload.close();
			globalThis.WebTransport = original;
		}
	});
}

/**
 * Stub WebTransport for lite-06, which carries the credential in the URL: refuses `refused`
 * once `release` resolves, and accepts any other credential, recording each dialed jwt.
 */
function refusing(refused: string, release: Promise<void>) {
	const dialed: (string | null)[] = [];
	const stub = function StubWebTransport(target: string | URL) {
		const jwt = new URL(target).searchParams.get("jwt");
		dialed.push(jwt);
		const pair = createMockTransportPair(Lite.ALPN_06);
		void accept(pair.server, new URL(target)).then(async () => {
			if (jwt !== refused) return;
			await release;
			pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		});
		return pair.client;
	};
	return { dialed, stub: stub as unknown as typeof WebTransport };
}

test("a refused credential replaced since its dial redials with the new one", async () => {
	const original = globalThis.WebTransport;
	const { promise: release, resolve } = Promise.withResolvers<void>();
	const { dialed, stub } = refusing("old", release);
	globalThis.WebTransport = stub;

	const tokens = new Signal<readonly string[]>(["old"]);
	const reload = new Reload({
		url: new URL("https://example.com/"),
		versions: ["moq-lite-06"],
		websocket: { enabled: false },
		tokens,
		delay: { initial: Milli(1), multiplier: 2, max: Milli(1), timeout: Milli(0) },
	});
	try {
		await waitUntil(() => reload.established.peek() !== undefined);
		tokens.set(["new"]);
		resolve();
		await waitUntil(() => dialed.includes("new"));
		expect(dialed).toEqual(["old", "new"]);
		await waitUntil(() => reload.established.peek() !== undefined);
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("new tokens redial a URL that stopped on refusing the old ones", async () => {
	const original = globalThis.WebTransport;
	const { dialed, stub } = refusing("old", Promise.resolve());
	globalThis.WebTransport = stub;

	const tokens = new Signal<readonly string[]>(["old"]);
	const reload = new Reload({
		url: new URL("https://example.com/"),
		versions: ["moq-lite-06"],
		websocket: { enabled: false },
		tokens,
		delay: { initial: Milli(1), multiplier: 2, max: Milli(1), timeout: Milli(0) },
	});
	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		expect(dialed).toEqual(["old"]);
		tokens.set(["new"]);
		await waitUntil(() => dialed.includes("new"));
		await waitUntil(() => reload.established.peek() !== undefined);
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("an unauthorized session close during setup stops retrying", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	let attempts = 0;
	const stub = function StubWebTransport() {
		attempts++;
		const pair = createMockTransportPair("");
		void (async () => {
			const incoming = pair.server.incomingBidirectionalStreams.getReader();
			const accepted = await incoming.read();
			incoming.releaseLock();
			if (accepted.done) return;

			// Some transports reject the SETUP stream before publishing the close info.
			await accepted.value.writable.abort(toTransport(StreamCode.DeliveryTimeout, "session closing"));
			await settle();
			await settle();
			pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		})();
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		// Unlimited retries prove that `closed` settles only because the rejection is terminal.
		delay: { initial: Milli(1), multiplier: 1, max: Milli(1), timeout: Milli(0) },
	});

	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		const err = reload.error.peek();
		expect(err).toBeInstanceOf(SessionError);
		expect((err as SessionError).code).toBe(SessionCode.Unauthorized);
		expect(attempts).toBe(1);
		expect(reload.closed.peek()).toBeUndefined();

		// A new URL starts another sequence on the same loop.
		reload.url.set(new URL("https://example.com/changed"));
		await waitUntil(() => attempts >= 2);
		expect(attempts).toBeGreaterThanOrEqual(2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

// SessionCode.Unauthorized and StreamCode.DeliveryTimeout are both 2, in registries that are
// disjoint. Reading a stream reset off the session table would call every SETUP-stream reset
// an auth rejection and suppress reconnect for the life of the page.
test("a setup stream reset is not mistaken for an unauthorized session", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	let attempts = 0;
	const stub = function StubWebTransport() {
		// No ALPN, so the handshake exchanges SETUP over a bidi stream we can reset.
		const pair = createMockTransportPair("");
		void (async () => {
			const incoming = pair.server.incomingBidirectionalStreams.getReader();
			const accepted = await incoming.read();
			incoming.releaseLock();
			if (accepted.done) return;

			attempts++;
			await accepted.value.writable.abort(toTransport(StreamCode.DeliveryTimeout, "delivery timeout"));
		})();
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		// Unlimited retries, so a second attempt can only happen because the first failure
		// was treated as retryable rather than terminal.
		delay: { initial: Milli(1), multiplier: 1, max: Milli(1), timeout: Milli(0) },
	});

	try {
		await waitUntil(() => attempts >= 2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("origins span reconnects: local re-announces, remote re-populates", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/origins");

	// What the client publishes (persistent) and what it discovers (per session).
	const publishOrigin = new OriginProducer();
	const subscribeOrigin = new OriginProducer();
	publish(publishOrigin, Path.from("mine"));

	// Each connect attempt gets a fresh server session that publishes "remote" and records
	// what the client announced to it.
	const servers: { session: { abort: () => void }; saw: OriginProducer; watching: { close(): void } }[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const saw = new OriginProducer();
		// The server watches everything, so it asks the client for announcements.
		const watching = saw.announced();
		const serverOrigin = new OriginProducer();
		void accept(pair.server, url, { publish: serverOrigin.consume(), consume: saw }).then((session) => {
			publish(serverOrigin, Path.from("remote"));
			servers.push({ session, saw, watching });
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Milli(10), multiplier: 1, max: Milli(10) },
		publish: publishOrigin.consume(),
		consume: subscribeOrigin,
	});
	const reader = subscribeOrigin.consume();
	// A watch held across the reconnect: each session re-opens its announce request.
	const watching = reader.announced();

	try {
		// First session: the server's broadcast lands in the client origin, and the client's
		// publish lands in the server's.
		await waitUntil(() => wireOf(reader).routes(Path.from("remote")));
		await waitUntil(() => (servers[0] ? wireOf(servers[0].saw).routes(Path.from("mine")) : false));

		// Kill the session: the remote entry retracts, the local publish stays put.
		servers[0]?.session.abort();
		await waitUntil(() => !wireOf(reader).routes(Path.from("remote")));

		// The reconnect re-announces the (untouched) publish and re-populates the table.
		await waitUntil(() => servers.length > 1);
		await waitUntil(() => wireOf(reader).routes(Path.from("remote")));
		await waitUntil(() => (servers[1] ? wireOf(servers[1].saw).routes(Path.from("mine")) : false));
	} finally {
		watching.close();
		for (const server of servers) server.watching.close();
		reload.close();
		publishOrigin.close();
		subscribeOrigin.close();
		globalThis.WebTransport = original;
	}
});

// Once enabled, hold the FIN acknowledgement of every group stream `transport` opens, after the
// bytes and FIN reached the peer: a final group whose tail is still in flight.
function holdFins(transport: MockTransport) {
	const reached = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let enabled = false;
	const create = transport.createUnidirectionalStream.bind(transport);
	transport.createUnidirectionalStream = async (options) => {
		const writer = (await create(options)).getWriter();
		return new WritableStream<Uint8Array>({
			write: (bytes) => writer.write(bytes),
			abort: (reason) => writer.abort(reason),
			async close() {
				await writer.close();
				if (!enabled) return;
				reached.resolve();
				await release.promise;
			},
		});
	};
	return {
		reached: reached.promise,
		enable: () => {
			enabled = true;
		},
		release: release.resolve,
	};
}

// Releasing the last handle is ordinary teardown, so a track that finished just before keeps its
// final group, whether the session is serving or a predecessor still draining after a GOAWAY.
// A page going away aborts: nothing would run to finish the drain. `linger` is how long after
// `end` the session starts closing: the pool's two-second window, or at once for a private loop.
// `hide` then sends the page away mid-drain, after the loop itself has closed.
const teardowns = {
	"release of the last handle": {
		drains: true,
		goaway: false,
		linger: Milli(2000),
		hide: false,
		end: (handle: Connection) => handle.close(),
	},
	disable: {
		drains: true,
		goaway: false,
		linger: Milli(2000),
		hide: false,
		end: (handle: Connection) => handle.enabled.set(false),
	},
	"release after a GOAWAY": {
		drains: true,
		goaway: true,
		linger: Milli(0),
		hide: false,
		end: (handle: Connection) => handle.close(),
	},
	pagehide: {
		drains: false,
		goaway: false,
		linger: Milli(0),
		hide: false,
		end: (_: Connection, win: EventTarget) => win.dispatchEvent(new Event("pagehide")),
	},
	"pagehide during a release's drain": {
		drains: false,
		goaway: false,
		linger: Milli(2000),
		hide: true,
		end: (handle: Connection) => handle.close(),
	},
};

for (const [name, teardown] of Object.entries(teardowns)) {
	test(`teardown by ${name} ${teardown.drains ? "drains" : "aborts"} the session`, async () => {
		const original = globalThis.WebTransport;
		const previousWindow = globalThis.window;
		const previousDocument = globalThis.document;
		const win = new EventTarget();
		const doc = Object.assign(new EventTarget(), { hidden: false });
		Object.assign(globalThis, { window: win, document: doc });
		// The linger and close deadline pass only when the test advances them, so the held FIN
		// can't race the deadline.
		const clock = new Manual();
		const installed = clock.install();

		// Only the first session matters: the subscription lives on it.
		const url = new URL("https://example.com/");
		const dials: { server: Promise<Established>; quic: WebTransport }[] = [];
		const fin = { hold: undefined as ReturnType<typeof holdFins> | undefined, closed: false };
		const stub = function StubWebTransport() {
			const pair = createMockTransportPair(Lite.ALPN_06);
			if (dials.length === 0) {
				fin.hold = holdFins(pair.client);
				void pair.client.closed.then(() => {
					fin.closed = true;
				});
			}
			dials.push({ server: accept(pair.server, url), quic: pair.server });
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;

		// GOAWAY tuning takes a private loop; the rest share the pool, closing when the linger ends.
		const handle = new Connection(
			teardown.goaway ? { url, delay: { initial: Milli(0) }, goaway: { handover: Milli(60_000) } } : { url },
		);
		await waitUntil(() => handle.origin.peek() !== undefined);
		const origin = handle.origin.peek();
		if (!origin) throw new Error("no origin");
		const broadcastPublication = origin.createBroadcast(Path.from("room"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		await waitUntil(() => dials.length > 0);
		const first = dials[0];
		if (!first) throw new Error("no first dial");
		const server = await first.server;
		const remote = wireOf(server).consume(Path.from("room"));
		const track = remote.track("video").subscribe().ordered();

		try {
			producer.appendGroup().writeFrame(textFrame("first"));
			expect(await (await track.nextGroup())?.readString()).toBe("first");

			if (teardown.goaway) {
				const stream = await Stream.open(first.quic, { version: Lite.Version.DRAFT_06 });
				await stream.writer.u53(Lite.StreamId.Goaway);
				await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_06);
				await waitUntil(() => dials.length === 2 && handle.status.peek() === "connected");
			}

			fin.hold?.enable();
			const last = producer.appendGroup();
			last.writeFrame(textFrame("last"));
			last.finish();
			producer.finish();
			producer.close();
			teardown.end(handle, win);
			await clock.advance(Nano.fromMilli(teardown.linger));
			if (teardown.hide) win.dispatchEvent(new Event("pagehide"));

			if (teardown.drains) {
				expect(await (await track.nextGroup())?.readString()).toBe("last");
				await fin.hold?.reached;
				// Just short of the close deadline, the session still waits for the FIN.
				await clock.advance(Nano.fromMilli(Milli(999)));
				expect(fin.closed).toBe(false);
				fin.hold?.release();
			}
			await waitUntil(() => fin.closed);
		} finally {
			fin.hold?.release();
			track.close();
			remote.close();
			handle.close();
			resetShared();
			for (const dial of dials) (await dial.server).abort();
			broadcast.close();
			installed.close();
			globalThis.WebTransport = original;
			Object.assign(globalThis, { window: previousWindow, document: previousDocument });
		}
	});
}

// A live track holds the old session's close until the deadline. The auth handle backs one
// active session at a time, so the closing one must give it up for the replacement to dial.
// The page going away mid-drain aborts it too, since nothing would run to finish it.
const drainEnds = {
	"the close deadline": (clock: Manual, _: EventTarget) => clock.advance(Nano.fromMilli(Milli(1000))),
	pagehide: async (_: Manual, win: EventTarget) => {
		win.dispatchEvent(new Event("pagehide"));
	},
};

for (const [name, drainEnd] of Object.entries(drainEnds)) {
	test(`a URL change connects the replacement while the old session drains until ${name}`, async () => {
		const original = globalThis.WebTransport;
		const previousWindow = globalThis.window;
		const previousDocument = globalThis.document;
		const win = new EventTarget();
		Object.assign(globalThis, { window: win, document: Object.assign(new EventTarget(), { hidden: false }) });
		const clock = new Manual();
		const installed = clock.install();
		const dials: { server: Promise<Established>; closed: boolean }[] = [];
		const stub = function StubWebTransport(dialed: string | URL) {
			const pair = createMockTransportPair(Lite.ALPN_06);
			const dial = { server: accept(pair.server, new URL(dialed)), closed: false };
			void pair.client.closed.then(() => {
				dial.closed = true;
			});
			dials.push(dial);
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;

		const origin = new OriginProducer();
		const broadcastPublication = origin.createBroadcast(Path.from("room"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const reload = new Reload({
			url: new URL("https://example.com/a"),
			websocket: { enabled: false },
			publish: origin.consume(),
		});

		await waitUntil(() => dials.length > 0);
		const first = dials[0];
		if (!first) throw new Error("no first dial");
		const remote = wireOf(await first.server).consume(Path.from("room"));
		const track = remote.track("video").subscribe().ordered();
		try {
			// The group stays open, so the old session's drain waits out its deadline.
			producer.appendGroup().writeFrame(textFrame("live"));
			expect(await (await track.nextGroup())?.readString()).toBe("live");

			reload.url.set(new URL("https://example.com/b"));
			await waitUntil(() => dials.length === 2 && reload.status.peek() === "connected");
			expect(reload.error.peek()).toBeUndefined();
			expect(dials[0]?.closed).toBe(false);

			await drainEnd(clock, win);
			await waitUntil(() => dials[0]?.closed === true);
		} finally {
			track.close();
			remote.close();
			reload.close();
			for (const dial of dials) (await dial.server).abort();
			producer.finish();
			producer.close();
			broadcast.close();
			origin.close();
			installed.close();
			globalThis.WebTransport = original;
			Object.assign(globalThis, { window: previousWindow, document: previousDocument });
		}
	});
}

// Retiring a GOAWAY predecessor early drains it, but never past its handover window.
test("a retired GOAWAY predecessor still closes by its handover cap", async () => {
	const original = globalThis.WebTransport;
	const clock = new Manual();
	const installed = clock.install();
	const dials: { server: Promise<Established>; quic: WebTransport; closed: boolean }[] = [];
	const stub = function StubWebTransport(dialed: string | URL) {
		const pair = createMockTransportPair(Lite.ALPN_06);
		const dial = { server: accept(pair.server, new URL(dialed)), quic: pair.server, closed: false };
		void pair.client.closed.then(() => {
			dial.closed = true;
		});
		dials.push(dial);
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const reload = new Reload({
		url: new URL("https://example.com/"),
		websocket: { enabled: false },
		publish: origin.consume(),
		delay: { initial: Milli(0) },
		goaway: { handover: Milli(100) },
	});

	await waitUntil(() => dials.length > 0);
	const first = dials[0];
	if (!first) throw new Error("no first dial");
	const remote = wireOf(await first.server).consume(Path.from("room"));
	const track = remote.track("video").subscribe().ordered();
	try {
		// The group stays open, so a graceful close of the predecessor would wait out its deadline.
		producer.appendGroup().writeFrame(textFrame("live"));
		expect(await (await track.nextGroup())?.readString()).toBe("live");

		const stream = await Stream.open(first.quic, { version: Lite.Version.DRAFT_06 });
		await stream.writer.u53(Lite.StreamId.Goaway);
		await new Lite.Goaway("").encode(stream.writer, Lite.Version.DRAFT_06);
		await waitUntil(() => dials.length === 2 && reload.status.peek() === "connected");

		await clock.advance(Nano.fromMilli(Milli(90)));
		reload.close();
		await clock.advance(Nano.fromMilli(Milli(9)));
		expect(first.closed).toBe(false);
		await clock.advance(Nano.fromMilli(Milli(1)));
		await waitUntil(() => first.closed);
	} finally {
		track.close();
		remote.close();
		reload.close();
		for (const dial of dials) (await dial.server).abort();
		producer.finish();
		producer.close();
		broadcast.close();
		origin.close();
		installed.close();
		globalThis.WebTransport = original;
	}
});

// A publication only a token's grant covers keeps that grant while the session drains, so
// closing the loop ends the tokens after the drain rather than cutting the final group.
test("closing the loop keeps the tokens until the session drains", async () => {
	const original = globalThis.WebTransport;
	const clock = new Manual();
	const installed = clock.install();
	const url = new URL("https://example.com/");
	const dials: { server: Promise<Established>; quic: WebTransport }[] = [];
	const fin = { hold: undefined as ReturnType<typeof holdFins> | undefined, closed: false };
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_07_WIP);
		fin.hold = holdFins(pair.client);
		void pair.client.closed.then(() => {
			fin.closed = true;
		});
		// The connection's own credential publishes nothing; the token publishes anything.
		const server = accept(pair.server, url);
		void server.then(async (session) => {
			const requests = session.auth.requests();
			for (;;) {
				const request = await requests.next();
				if (!request) break;
				const token = new TextDecoder().decode(request.token);
				request.accept({
					publish: new Path.Patterns(token === "token" ? [Path.Pattern.all()] : []),
					subscribe: new Path.Patterns(),
				});
			}
		});
		dials.push({ server, quic: pair.server });
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const origin = new OriginProducer();
	const reload = new Reload({ url, websocket: { enabled: false }, publish: origin.consume() });
	await reload.auth.add("token");
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const first = dials[0];
	if (!first) throw new Error("no first dial");
	const remote = wireOf(await first.server).consume(Path.from("room"));
	const track = remote.track("video").subscribe().ordered();
	try {
		producer.appendGroup().writeFrame(textFrame("first"));
		expect(await (await track.nextGroup())?.readString()).toBe("first");

		fin.hold?.enable();
		const last = producer.appendGroup();
		last.writeFrame(textFrame("last"));
		last.finish();
		producer.finish();
		producer.close();
		reload.close();

		expect(await (await track.nextGroup())?.readString()).toBe("last");
		await fin.hold?.reached;
		await clock.advance(Nano.fromMilli(Milli(999)));
		expect(fin.closed).toBe(false);
		fin.hold?.release();
		await waitUntil(() => fin.closed);
	} finally {
		fin.hold?.release();
		track.close();
		remote.close();
		reload.close();
		for (const dial of dials) (await dial.server).abort();
		broadcast.close();
		origin.close();
		installed.close();
		globalThis.WebTransport = original;
	}
});

test("closing an announce consumer during upstream teardown does not append retractions", async () => {
	const { Producer } = await import("../announced.ts");
	const upstream = new Producer();
	const producer = new Producer();
	const pump = new Effect((effect) => pumpAnnounced(effect, upstream.consume(), producer));
	const consumer = producer.consume();
	// The same lifecycle as Connection.announced: the last consumer closing closes the producer and its pump.
	closeUnwanted(producer);
	void producer.closed.then(() => pump.close());
	const errors = spyOn(console, "error").mockImplementation(() => {});
	try {
		upstream.append({
			prefix: Path.from("alice/camera.hang"),
			captures: undefined,
			kind: "start",
			route: Route.default,
		});
		await consumer.next();
		upstream.close();
		// Let the upstream read settle, but close before the pump's finally callback runs.
		await Promise.resolve();
		consumer.close();
		await settle();
		expect(producer.closed.peek()).not.toBeUndefined();
		expect(errors.mock.calls).toEqual([]);
	} finally {
		consumer.close();
		pump.close();
		producer.close();
		errors.mockRestore();
	}
});

test("announced accepts a non-prefix scope before any session exists", () => {
	const connection = new Connection({ enabled: false });
	try {
		const announced = connection.announced(Path.Pattern.parse("room/*"));
		announced.close();
	} finally {
		connection.close();
	}
});

/**
 * A watcher on the consume origin syncs only once the session has delivered the
 * server's initial set, exactly on versions that count it (lite-05+, MoQ Active Count), and
 * a reconnect takes it back until the next session's set lands.
 */
test("a watcher syncs with the server's initial set, and again after a reconnect", async () => {
	// Through the consume origin, and through the handle's own stream without one.
	for (const [alpn, viaOrigin] of [
		[Lite.ALPN_06, true],
		[IetfALPN.DRAFT_18, true],
		[Lite.ALPN_06, false],
		[IetfALPN.DRAFT_18, false],
	] as const) {
		const original = globalThis.WebTransport;
		const url = new URL("https://example.com/");

		const serverOrigin = new OriginProducer();
		const published = [publish(serverOrigin, Path.from("a")), publish(serverOrigin, Path.from("b"))];
		const sessions: { abort: () => void }[] = [];
		const stub = function StubWebTransport() {
			const pair = createMockTransportPair(alpn);
			void accept(pair.server, url, { publish: serverOrigin.consume() }).then((server) => {
				sessions.push(server);
			});
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;

		const clientOrigin = new OriginProducer();
		const props = {
			enabled: true,
			url,
			websocket: { enabled: false },
			delay: { initial: Milli(10), multiplier: 1, max: Milli(10) },
		};
		const reload = viaOrigin ? new Reload({ ...props, consume: clientOrigin }) : undefined;
		const connection = viaOrigin ? undefined : new Connection({ ...props, share: false });
		const watching = connection ? connection.announced() : clientOrigin.announced();
		const seen = new Set<string>();
		void (async () => {
			for await (const event of watching) {
				if (event.kind === "end") seen.delete(event.prefix);
				else seen.add(event.prefix);
			}
		})();

		try {
			// Nothing is connected yet, which is not the same as nothing being announced.
			expect(watching.synced.peek()).toBe(false);
			await waitUntil(() => watching.synced.peek());
			expect([...seen].sort()).toEqual(["a", "b"]);

			sessions[0]?.abort();
			await waitUntil(() => !watching.synced.peek());
			await waitUntil(() => sessions.length > 1 && watching.synced.peek());
			expect([...seen].sort()).toEqual(["a", "b"]);
		} finally {
			watching.close();
			reload?.close();
			connection?.close();
			clientOrigin.close();
			for (const broadcast of published) broadcast.close();
			serverOrigin.close();
			for (const session of sessions) session.abort();
			globalThis.WebTransport = original;
		}
	}
});

/** Replacing a synced upstream with one already synced still owes the old routes' ends. */
test("a pump owes the retractions of the upstream it replaces", async () => {
	const tick = async () => {
		for (let i = 0; i < 16; i++) await Promise.resolve();
	};
	const producer = new Announce.Producer();
	const consumer = producer.consume();

	const first = new Announce.Producer();
	first.append({ prefix: Path.from("a"), captures: undefined, kind: "start", route: Route.default });
	first.synced();
	const upstream = new Signal(first);
	const pump = new Effect((effect) => pumpAnnounced(effect, effect.get(upstream).consume(), producer));

	expect(await consumer.next()).toMatchObject({ prefix: Path.from("a"), kind: "start" });
	const parked = consumer.next();
	await tick();
	expect(consumer.synced.peek()).toBe(true);

	const second = new Announce.Producer();
	second.synced();
	upstream.set(second);
	expect(await parked).toMatchObject({ prefix: Path.from("a"), kind: "end" });
	await tick();
	expect(consumer.synced.peek()).toBe(false);
	void consumer.next();
	await tick();
	expect(consumer.synced.peek()).toBe(true);

	pump.close();
	producer.close();
});

test("redirect loop detection catches any cycle in constant memory", () => {
	for (let tail = 0; tail < 4; tail++) {
		for (let cycle = 2; cycle <= 8; cycle++) {
			// `tail` one-way moves, then `cycle` endpoints redirecting around a ring.
			const at = (hop: number) => (hop < tail ? `tail${hop}` : `ring${(hop - tail) % cycle}`);
			const redirects = new Redirects();
			let caught: number | undefined;
			for (let hop = 0; hop < 4 * (tail + cycle); hop++) {
				const loop = redirects.move(at(hop), at(hop + 1));
				if (caught !== undefined) expect(loop, "a caught loop stays caught").toBe(true);
				else if (loop) caught = hop;
			}
			const label = `tail ${tail}, cycle ${cycle}`;
			expect(caught, label).toBeDefined();
			// Never before the run first returns to an endpoint it left.
			expect(caught, label).toBeGreaterThanOrEqual(tail + cycle - 1);
		}
	}

	const redirects = new Redirects();
	for (let hop = 0; hop < 1000; hop++) expect(redirects.move(`a${hop}`, `a${hop + 1}`)).toBe(false);
});

// Fire the installed Manual clock timer by timer until `pred` holds; returns the Manual time
// that passed, in milliseconds.
async function elapse(clock: Manual, pred: () => boolean): Promise<number> {
	const start = clock.now();
	for (;;) {
		for (let i = 0; i < 3 && !pred(); i++) await settle();
		if (pred()) return Nano.toMilli(Instant.durationSince(clock.now(), start));
		await clock.fire();
	}
}

// Stub WebTransport so each dial either hangs, as one placed on a dead target does, or
// connects to a relay that accepts it.
function dialPlan(plan: (dial: number) => "hang" | "ok", url: URL) {
	const original = globalThis.WebTransport;
	const origin = new OriginProducer();
	let dials = 0;
	let hungCloses = 0;
	class Hung {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});
		close() {
			hungCloses++;
		}
	}
	globalThis.WebTransport = function StubWebTransport() {
		const dial = dials++;
		if (plan(dial) === "hang") return new Hung();
		const pair = createMockTransportPair(Lite.ALPN_06);
		void accept(pair.server, url, { publish: origin.consume() });
		return pair.client;
	} as unknown as typeof WebTransport;
	return {
		dials: () => dials,
		hungCloses: () => hungCloses,
		restore: () => {
			globalThis.WebTransport = original;
			origin.close();
		},
	};
}

test("an attempt that outlives the connect timeout is retried after the backoff", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const url = new URL("https://relay.example/room");
	// Scaled down: the attempt times out before a WebTransport hedge would dial.
	const plan = dialPlan((dial) => (dial === 0 ? "hang" : "ok"), url);
	const statuses: string[] = [];
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		timeout: Milli(50),
		delay: { initial: Milli(20) },
	});
	const unsubscribe = reload.status.subscribe((status) => statuses.push(status));
	try {
		await waitUntil(() => plan.dials() === 1);
		const waited = await elapse(clock, () => plan.dials() === 2);
		// The 50 ms timeout, then the first backoff with its equal jitter (10 to 20 ms).
		expect(waited).toBeGreaterThanOrEqual(60);
		expect(waited).toBeLessThanOrEqual(70);
		expect(plan.hungCloses()).toBe(1);

		await waitUntil(() => reload.status.peek() === "connected");
		expect(statuses).toContain("disconnected");
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		unsubscribe();
		reload.close();
		plan.restore();
		installed.close();
	}
});

// Every attempt hangs until its 30 ms connect timeout. The window counts from the first dial, as
// moq-tokio's does, and no attempt starts once it has passed.
const windows: [string, ReloadDelay, number, number][] = [
	// Dials at 0, 40, and 80; the last runs past the window and gives up when it fails.
	["an attempt under way finishes past the window", { initial: Milli(10), timeout: Milli(100) }, 110, 3],
	// The first failure at 30 ms leaves 30 ms, less than the 50 ms wait, so it gives up at the deadline.
	["a wait that reaches the deadline gives up there", { initial: Milli(50), timeout: Milli(60) }, 60, 1],
];
for (const [name, delay, gaveUp, dials] of windows) {
	test(`retry window: ${name}`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		// Full delay every time, so the waits are exact.
		const random = spyOn(Math, "random").mockReturnValue(1);
		const warned = spyOn(console, "warn").mockImplementation(() => {});
		const url = new URL("https://relay.example/room");
		const plan = dialPlan(() => "hang", url);
		const reload = new Reload({
			url,
			websocket: { enabled: false },
			timeout: Milli(30),
			delay: { multiplier: 1, max: Milli(1000), ...delay },
		});
		try {
			await waitUntil(() => plan.dials() === 1);
			expect(await elapse(clock, () => reload.error.peek() !== undefined)).toBe(gaveUp);
			expect(reload.error.peek()).toBeInstanceOf(ConnectTimeout);
			expect(plan.dials()).toBe(dials);
			expect(plan.hungCloses()).toBe(plan.dials());
		} finally {
			reload.close();
			plan.restore();
			warned.mockRestore();
			random.mockRestore();
			installed.close();
		}
	});
}

test("an invalid timeout is refused when the loop is built, not by every attempt", () => {
	for (const timeout of [-1, Number.NaN, Number.POSITIVE_INFINITY]) {
		expect(() => new Reload({ url: new URL("https://example.com/"), timeout: timeout as Milli })).toThrow(
			RangeError,
		);
	}
});

// Each session lives 100 ms, so the backoff between them shows how `healthy` scores its end and
// how `max` caps the delay, matching moq-tokio's `connect.backoff`.
const backoffs: [string, ReloadDelay, number[]][] = [
	// Past `initial` but short of the default bar, so each end is a failed attempt.
	["a session short of the default healthy bar escalates", {}, [10, 20, 40]],
	["a session past the healthy bar resets", { healthy: Milli(50) }, [10, 10, 10]],
	[
		"a zero max leaves the delay uncapped",
		{ initial: Milli(1000), multiplier: 4, max: Milli(0) },
		[1000, 4000, 16000],
	],
	["a max below initial caps the first delay too", { max: Milli(5) }, [5, 5, 5]],
	["a zero multiplier keeps the delay constant", { multiplier: 0 }, [10, 10, 10]],
	["a fractional multiplier below 1 keeps the delay constant", { multiplier: 0.5 }, [10, 10, 10]],
];
for (const [name, delay, gaps] of backoffs) {
	test(`backoff: ${name}`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		// Full delay every time, so the waits are exact.
		const random = spyOn(Math, "random").mockReturnValue(1);
		const warned = spyOn(console, "warn").mockImplementation(() => {});
		const original = globalThis.WebTransport;
		const url = new URL("https://relay.example/room");
		const servers: Promise<{ abort(): void }>[] = [];
		globalThis.WebTransport = function StubWebTransport() {
			const pair = createMockTransportPair(Lite.ALPN_06);
			servers.push(accept(pair.server, url));
			return pair.client;
		} as unknown as typeof WebTransport;
		const reload = new Reload({
			url,
			websocket: { enabled: false },
			delay: { initial: Milli(10), multiplier: 2, max: Milli(1000), ...delay },
		});
		try {
			const waited: number[] = [];
			for (let session = 0; session < gaps.length; session++) {
				await waitUntil(() => reload.status.peek() === "connected");
				await clock.advance(Nano.fromMilli(Milli(100)));
				(await servers[session])?.abort();
				waited.push(await elapse(clock, () => servers.length === session + 2));
			}
			expect(waited).toEqual([...gaps]);
		} finally {
			reload.close();
			globalThis.WebTransport = original;
			warned.mockRestore();
			random.mockRestore();
			installed.close();
		}
	});
}

// The delay after the second failure, 1e601 ms, overflows; it saturates, as Rust's Duration
// does, rather than failing the loop. The 1e301 ms wait itself is never fired: at that
// instant a 100 ms poll no longer advances the clock.
test("backoff: an uncapped delay saturates instead of overflowing", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const failed = spyOn(console, "error");
	const original = globalThis.WebTransport;
	const url = new URL("https://relay.example/room");
	const servers: Promise<{ abort(): void }>[] = [];
	globalThis.WebTransport = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06);
		servers.push(accept(pair.server, url));
		return pair.client;
	} as unknown as typeof WebTransport;
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		delay: { initial: Milli(10), multiplier: 1e300, max: Milli(0) },
	});
	try {
		for (let session = 0; session < 2; session++) {
			await waitUntil(() => reload.status.peek() === "connected");
			(await servers[session])?.abort();
			if (session === 0) await elapse(clock, () => servers.length === 2);
		}
		await waitUntil(() => reload.status.peek() === "disconnected");
		for (let i = 0; i < 10; i++) await settle();
		expect(failed).not.toHaveBeenCalled();
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
		failed.mockRestore();
		warned.mockRestore();
		installed.close();
	}
});

test("a delay field with no meaning is refused when the loop is built", () => {
	const bad = [-1, Number.NaN, Number.POSITIVE_INFINITY] as Milli[];
	const invalid: ReloadDelay[] = [
		// Zero would count every session as healthy.
		{ healthy: Milli(0) },
		// Zero `max` is uncapped and zero `timeout` unlimited, so infinity has no spelling here.
		...bad.flatMap((value) => [{ initial: value }, { max: value }, { healthy: value }, { timeout: value }]),
		{ multiplier: Number.NaN },
		{ multiplier: Number.POSITIVE_INFINITY },
	];
	for (const delay of invalid) {
		expect(() => new Reload({ url: new URL("https://example.com/"), delay }), JSON.stringify(delay)).toThrow(
			RangeError,
		);
	}
});
