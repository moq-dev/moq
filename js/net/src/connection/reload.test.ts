import { expect, spyOn, test } from "bun:test";
import { Duration, type Instant, now, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Session as Qmux, type WebSocketStreamLike } from "@moq/qmux";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import { Viewers } from "../bandwidth.ts";
import type { Producer as BroadcastProducer } from "../broadcast.ts";
import { ConnectTimeout, SessionCode, SessionError, StreamCode, toTransport } from "../error.ts";
import { WireRoute } from "../hop.ts";
// Loaded up front: a session imports it on first use, which takes host turns a Manual clock cannot wait out.
import "../ietf/connection.ts";
import { ALPN as IetfALPN } from "../ietf/version.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, type MockTransport, manual, textFrame, waitFor } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { Timescale } from "../time.ts";
import { closeUnwanted } from "../util/announce.ts";
import { wireOf } from "../wire.ts";
import { type AcceptProps, accept as acceptSession } from "./index.ts";
import { Connection, resetShared } from "./pool.ts";
import { pumpAnnounced, Redirects, Reload, type ReloadDelay, type ReloadProps } from "./reload.ts";
import type { Session } from "./session.ts";

function accept(transport: WebTransport, url: URL, props: Omit<AcceptProps, "transport" | "url"> = {}) {
	return acceptSession({ transport, url, ...props });
}

function publish(origin: OriginProducer, path: Path.Valid) {
	const broadcastRoute = origin.route(path);
	const broadcast = broadcastRoute.broadcast;
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

test("ReloadProps excludes viewers", () => {
	const withViewers = { viewers: new Viewers() };
	// @ts-expect-error a reload owns its viewers registry
	const props: ReloadProps = withViewers;
	expect(props.enabled).toBeUndefined();
	withViewers.viewers.close();
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
	using clock = manual();
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
		delay: {
			initial: Duration.millis(1000),
			multiplier: 2,
			max: Duration.millis(1000),
			timeout: Duration.millis(1),
		},
	});
	try {
		await clock.drive(waitFor(reload.error, (error) => error !== undefined));
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
	using clock = manual();
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
		delay: { initial: undefined, multiplier: undefined, max: undefined, timeout: null },
	});
	try {
		await clock.advance(Duration.millis(100));
		// The default initial delay is 1000ms, so the first retry is still pending.
		expect(dials).toBe(1);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

// The paths `routes` has announced and not retracted since, read as they arrive.
function present(routes: Announce.Routes): Getter<ReadonlySet<Path.Valid>> {
	const paths = new Signal<ReadonlySet<Path.Valid>>(new Set());
	void (async () => {
		for await (const event of routes) {
			paths.update((previous) => {
				const next = new Set(previous);
				if (event.kind === "end" || event.kind === "offline") next.delete(event.prefix);
				else next.add(event.prefix);
				return next;
			});
		}
	})();
	return paths;
}

// Polls until `pred` holds, so a regression fails the test instead of hanging it.
async function waitUntil(pred: () => boolean): Promise<void> {
	const deadline = now().saturatingAdd(Duration.millis(1000));
	while (now().le(deadline)) {
		if (pred()) return;
		await settle();
	}
	throw new Error("timed out waiting for condition");
}

test("a follower follows the reconnect on the same handle", async () => {
	using clock = manual();
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
		delay: { initial: Duration.millis(10), multiplier: 1, max: Duration.millis(10) },
		consume: clientOrigin,
	});
	const watched = clientOrigin.follow(Path.from("late"));

	try {
		const first = await clock.drive(waitFor(watched.active, (active) => active !== undefined));

		// The session dies; the same request follows the next session's announcement.
		sessions[0]?.abort();
		await clock.drive(waitFor(watched.active, (active) => active !== undefined && active !== first));
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
			await clock.advance(Duration.millis(1));
		}
		expect(done()).toBe(true);
	};

	// An unbounded retry window, so only the ended handle can stop the loop.
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Duration.millis(1), multiplier: 2, max: Duration.millis(1), timeout: null },
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

test("a page hide after give-up does not retry the failed URL", async () => {
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
		// Refused before any session, so the URL's first attempt fails and the loop gives up.
		const pair = createMockTransportPair(Lite.ALPN_06);
		pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Duration.millis(1), multiplier: 1, max: Duration.millis(1), timeout: null },
	});

	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		const givenUp = attempts;
		const err = reload.error.peek();

		// Suspension shares sequence reset with give-up; resume must not look like a new
		// sequence or it would clear `error` and redial a URL that already gave up.
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
	test(`a session rejected with ${code} surfaces the code and redials`, async () => {
		using clock = manual();
		const original = globalThis.WebTransport;
		const url = new URL("https://example.com/");
		const closes: (Error | null)[] = [];
		let server: MockTransport | undefined;
		const dials = new Signal(0);
		const stub = function StubWebTransport() {
			dials.update((n) => n + 1);
			const pair = createMockTransportPair(Lite.ALPN_06);
			server = pair.server;
			void accept(pair.server, url);
			return pair.client;
		};
		globalThis.WebTransport = stub as unknown as typeof WebTransport;

		const reload = new Reload({
			enabled: true,
			url,
			websocket: { enabled: false },
			delay: { initial: Duration.millis(1), multiplier: 2, max: Duration.millis(1), timeout: null },
		});
		const watch = new Effect();
		watch.run((effect) => {
			const conn = effect.get(reload.established);
			if (conn) {
				effect.spawn(async () => {
					closes.push(await conn.closed);
				});
				// Reject at the MoQ layer once both sides are set up, the way a relay's
				// Request::close does after it has already accepted the transport.
				server?.close({ closeCode: code, reason: "unauthorized" });
			}
		});

		try {
			// The code reaches the app rather than being flattened away, and the loop redials
			// like after any other close instead of judging the error's kind.
			await clock.drive(waitFor(dials, (n) => n >= 2));
			expect(closes.find((e) => e instanceof SessionError)).toBeInstanceOf(SessionError);
			expect((closes.find((e) => e instanceof SessionError) as SessionError).code).toBe(code);
			expect(reload.error.peek()).toBeUndefined();
			expect(reload.closed.peek()).toBeUndefined();
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
	const dialed = new Signal<readonly (string | null)[]>([]);
	const stub = function StubWebTransport(target: string | URL) {
		const jwt = new URL(target).searchParams.get("jwt");
		dialed.update((jwts) => [...jwts, jwt]);
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
	using clock = manual();
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
		delay: { initial: Duration.millis(1), multiplier: 2, max: Duration.millis(1), timeout: null },
	});
	try {
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		tokens.set(["new"]);
		resolve();
		await clock.drive(waitFor(dialed, (jwts) => jwts.includes("new")));
		expect(dialed.peek()).toEqual(["old", "new"]);
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("a refused credential keeps redialing until the tokens change", async () => {
	using clock = manual();
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
		delay: { initial: Duration.millis(1), multiplier: 2, max: Duration.millis(1), timeout: null },
	});
	try {
		// Refused only once a session is up: a URL's first attempt failing stops it whatever the cause.
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		resolve();
		await clock.drive(waitFor(dialed, (jwts) => jwts.length >= 3));
		expect(dialed.peek().every((jwt) => jwt === "old")).toBe(true);
		expect(reload.error.peek()).toBeUndefined();
		tokens.set(["new"]);
		await clock.drive(waitFor(dialed, (jwts) => jwts.includes("new")));
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("an unauthorized session close during the first setup stops retrying", async () => {
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
		tokens: ["old"],
		// Unlimited retries prove that `error` settles only because the first attempt failed.
		delay: { initial: Duration.millis(1), multiplier: 1, max: Duration.millis(1), timeout: null },
	});

	try {
		await waitUntil(() => reload.error.peek() !== undefined);
		const err = reload.error.peek();
		expect(err).toBeInstanceOf(SessionError);
		expect((err as SessionError).code).toBe(SessionCode.Unauthorized);
		expect(attempts).toBe(1);
		expect(reload.closed.peek()).toBeUndefined();

		// New credentials alone do not restart a URL that gave up.
		reload.tokens.set(["new"]);
		for (let i = 0; i < 20; i++) await settle();
		expect(attempts).toBe(1);

		// A new URL starts another sequence on the same loop.
		reload.url.set(new URL("https://example.com/changed"));
		await waitUntil(() => attempts >= 2);
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

// SessionCode.Unauthorized and StreamCode.DeliveryTimeout are both 2, in registries that are
// disjoint. Reading a stream reset off the session table would call every SETUP-stream reset
// an auth rejection and suppress reconnect for the life of the page.
test("a setup stream reset is not mistaken for an unauthorized session", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	const attempts = new Signal(0);
	let dials = 0;
	let first: Promise<{ abort(): void }> | undefined;
	const stub = function StubWebTransport() {
		// The first dial connects: a URL's first attempt failing stops it whatever the cause.
		if (dials++ === 0) {
			const pair = createMockTransportPair(Lite.ALPN_06);
			first = accept(pair.server, url);
			return pair.client;
		}
		// No ALPN, so the handshake exchanges SETUP over a bidi stream we can reset.
		const pair = createMockTransportPair("");
		void (async () => {
			const incoming = pair.server.incomingBidirectionalStreams.getReader();
			const accepted = await incoming.read();
			incoming.releaseLock();
			if (accepted.done) return;

			attempts.update((n) => n + 1);
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
		delay: { initial: Duration.millis(1), multiplier: 1, max: Duration.millis(1), timeout: null },
	});

	try {
		await clock.drive(waitFor(reload.established, (established) => established !== undefined));
		(await first)?.abort();
		await clock.drive(waitFor(attempts, (n) => n >= 2));
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		globalThis.WebTransport = original;
	}
});

test("origins span reconnects: local re-announces, remote re-populates", async () => {
	using clock = manual();
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/origins");

	// What the client publishes (persistent) and what it discovers (per session).
	const publishOrigin = new OriginProducer();
	const subscribeOrigin = new OriginProducer();
	publish(publishOrigin, Path.from("mine"));

	// Each connect attempt gets a fresh server session that publishes "remote" and records
	// what the client announced to it.
	const servers = new Signal<
		readonly { session: { abort: () => void }; watching: Announce.Routes; saw: Getter<ReadonlySet<Path.Valid>> }[]
	>([]);
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const saw = new OriginProducer();
		// The server watches everything, so it asks the client for announcements.
		const watching = saw.routes();
		const serverOrigin = new OriginProducer();
		void accept(pair.server, url, { publish: serverOrigin.consume(), consume: saw }).then((session) => {
			publish(serverOrigin, Path.from("remote"));
			servers.update((list) => [...list, { session, watching, saw: present(watching) }]);
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: Duration.millis(10), multiplier: 1, max: Duration.millis(10) },
		publish: publishOrigin.consume(),
		consume: subscribeOrigin,
	});
	const reader = subscribeOrigin.consume();
	// A watch held across the reconnect: each session re-opens its announce request.
	const watching = reader.routes();
	const remote = present(watching);

	try {
		// First session: the server's broadcast lands in the client origin, and the client's
		// publish lands in the server's.
		await clock.drive(waitFor(remote, (paths) => paths.has(Path.from("remote"))));
		const [first] = await clock.drive(waitFor(servers, (list) => list.length > 0));
		await clock.drive(waitFor(first.saw, (paths) => paths.has(Path.from("mine"))));

		// Kill the session: the remote entry retracts, the local publish stays put.
		first.session.abort();
		await clock.drive(waitFor(remote, (paths) => !paths.has(Path.from("remote"))));

		// The reconnect re-announces the (untouched) publish and re-populates the table.
		const [, second] = await clock.drive(waitFor(servers, (list) => list.length > 1));
		await clock.drive(waitFor(remote, (paths) => paths.has(Path.from("remote"))));
		await clock.drive(waitFor(second.saw, (paths) => paths.has(Path.from("mine"))));
	} finally {
		watching.close();
		for (const server of servers.peek()) server.watching.close();
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
		linger: Duration.millis(2000),
		hide: false,
		end: (handle: Connection) => handle.close(),
	},
	disable: {
		drains: true,
		goaway: false,
		linger: Duration.millis(2000),
		hide: false,
		end: (handle: Connection) => handle.enabled.set(false),
	},
	"release after a GOAWAY": {
		drains: true,
		goaway: true,
		linger: Duration.millis(0),
		hide: false,
		end: (handle: Connection) => handle.close(),
	},
	pagehide: {
		drains: false,
		goaway: false,
		linger: Duration.millis(0),
		hide: false,
		end: (_: Connection, win: EventTarget) => win.dispatchEvent(new Event("pagehide")),
	},
	"pagehide during a release's drain": {
		drains: false,
		goaway: false,
		linger: Duration.millis(2000),
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
		const dials: { server: Promise<Session>; quic: WebTransport }[] = [];
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
			teardown.goaway
				? { url, delay: { initial: Duration.millis(0) }, goaway: { handover: Duration.millis(60_000) } }
				: { url },
		);
		await waitUntil(() => handle.origin.peek() !== undefined);
		const origin = handle.origin.peek();
		if (!origin) throw new Error("no origin");
		const broadcastRoute = origin.route(Path.from("room"));
		const broadcast = broadcastRoute.broadcast;
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
			await clock.advance(teardown.linger);
			if (teardown.hide) win.dispatchEvent(new Event("pagehide"));

			if (teardown.drains) {
				expect(await (await track.nextGroup())?.readString()).toBe("last");
				await fin.hold?.reached;
				// Just short of the close deadline, the session still waits for the FIN.
				await clock.advance(Duration.millis(999));
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
	"the close deadline": (clock: Manual, _: EventTarget) => clock.advance(Duration.millis(1000)),
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
		const dials: { server: Promise<Session>; closed: boolean }[] = [];
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
		const broadcastRoute = origin.route(Path.from("room"));
		const broadcast = broadcastRoute.broadcast;
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
	const dials: { server: Promise<Session>; quic: WebTransport; closed: boolean }[] = [];
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
	const broadcastRoute = origin.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const reload = new Reload({
		url: new URL("https://example.com/"),
		websocket: { enabled: false },
		publish: origin.consume(),
		delay: { initial: Duration.millis(0) },
		goaway: { handover: Duration.millis(100) },
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

		await clock.advance(Duration.millis(90));
		reload.close();
		await clock.advance(Duration.millis(9));
		expect(first.closed).toBe(false);
		await clock.advance(Duration.millis(1));
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
	const dials: { server: Promise<Session>; quic: WebTransport }[] = [];
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
	await reload.auth.add("token").answered;
	const broadcastRoute = origin.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
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
		await clock.advance(Duration.millis(999));
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
			route: WireRoute.default,
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
		const announced = connection.routes(Path.Pattern.parse("room/*"));
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
	using clock = manual();
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
			delay: { initial: Duration.millis(10), multiplier: 1, max: Duration.millis(10) },
		};
		const reload = viaOrigin ? new Reload({ ...props, consume: clientOrigin }) : undefined;
		const connection = viaOrigin ? undefined : new Connection({ ...props, share: false });
		const watching = connection ? connection.routes() : clientOrigin.routes();
		const seen = new Set<string>();
		void (async () => {
			for await (const event of watching) {
				if (event.kind === "end" || event.kind === "offline") seen.delete(event.prefix);
				else seen.add(event.prefix);
			}
		})();

		try {
			// Nothing is connected yet, which is not the same as nothing being announced.
			expect(watching.synced.peek()).toBe(false);
			await clock.drive(waitFor(watching.synced, (synced) => synced));
			expect([...seen].sort()).toEqual(["a", "b"]);

			sessions[0]?.abort();
			await clock.drive(waitFor(watching.synced, (synced) => !synced));
			await clock.drive(waitFor(watching.synced, (synced) => synced));
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
	first.append({ prefix: Path.from("a"), captures: undefined, kind: "start", route: WireRoute.default });
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
	expect(await parked).toMatchObject({ prefix: Path.from("a"), kind: "offline" });
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
		if (pred()) return clock.now().since(start).asMillis();
		await clock.fire();
	}
}

// Stub WebTransport so each dial either hangs, as one placed on a dead target does, or
// connects to a relay that accepts it, which `drop` severs.
function dialPlan(plan: (dial: number) => "hang" | "ok") {
	const original = globalThis.WebTransport;
	const origin = new OriginProducer();
	const servers: Promise<{ abort(): void }>[] = [];
	let dials = 0;
	let hungCloses = 0;
	class Hung {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});
		close() {
			hungCloses++;
		}
	}
	globalThis.WebTransport = function StubWebTransport(target: string | URL) {
		const dial = dials++;
		if (plan(dial) === "hang") return new Hung();
		const pair = createMockTransportPair(Lite.ALPN_06);
		servers.push(accept(pair.server, new URL(target), { publish: origin.consume() }));
		return pair.client;
	} as unknown as typeof WebTransport;
	return {
		dials: () => dials,
		hungCloses: () => hungCloses,
		// Sever the latest accepted session from the server.
		drop: async () => (await servers.at(-1))?.abort(),
		restore: () => {
			globalThis.WebTransport = original;
			origin.close();
		},
	};
}

// Connect, hold the session past the default healthy bar, then sever it from the server. A URL's
// first attempt is never retried, so retrying needs a session first; ending a healthy one
// leaves the backoff and the retry window fresh, both counting from the drop.
async function outlive(clock: Manual, reload: Reload, plan: ReturnType<typeof dialPlan>) {
	await waitUntil(() => reload.status.peek() === "connected");
	await clock.advance(Duration.millis(1000));
	await plan.drop();
	await waitUntil(() => reload.status.peek() !== "connected");
}

test("a URL's first attempt failing stops it until the URL changes", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const plan = dialPlan((dial) => (dial === 0 ? "hang" : "ok"));
	const reload = new Reload({
		url: new URL("https://relay.example/room"),
		websocket: { enabled: false },
		timeout: Duration.millis(30),
		// An unbounded window and a short backoff, so only the first-attempt rule can stop it.
		delay: { initial: Duration.millis(10), multiplier: 1, max: Duration.millis(10), timeout: null },
	});
	try {
		await waitUntil(() => plan.dials() === 1);
		expect(await elapse(clock, () => reload.error.peek() !== undefined)).toBe(30);
		expect(reload.error.peek()).toBeInstanceOf(ConnectTimeout);
		expect(reload.status.peek()).toBe("disconnected");

		await clock.advance(Duration.millis(1000));
		expect(plan.dials()).toBe(1);
		expect(plan.hungCloses()).toBe(1);

		// A new URL needs its own first session, and dials it.
		reload.url.set(new URL("https://relay.example/other"));
		await waitUntil(() => reload.status.peek() === "connected");
		expect(plan.dials()).toBe(2);
		expect(reload.error.peek()).toBeUndefined();
	} finally {
		reload.close();
		plan.restore();
		warned.mockRestore();
		installed.close();
	}
});

test("an attempt that outlives the connect timeout is retried after the backoff", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const url = new URL("https://relay.example/room");
	// Scaled down: the attempt times out before a WebTransport hedge would dial.
	const plan = dialPlan((dial) => (dial === 1 ? "hang" : "ok"));
	const statuses: string[] = [];
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		timeout: Duration.millis(50),
		delay: { initial: Duration.millis(20), multiplier: 1 },
	});
	const unsubscribe = reload.status.subscribe((status) => statuses.push(status));
	try {
		await outlive(clock, reload, plan);
		await elapse(clock, () => plan.dials() === 2);
		statuses.length = 0;
		const waited = await elapse(clock, () => plan.dials() === 3);
		// The 50 ms timeout, then the backoff with its equal jitter (10 to 20 ms).
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

// A healthy session ends at 0, then every attempt hangs until its 30 ms connect timeout. The
// window counts from the end of that session, as moq-tokio's does, and no attempt starts once
// it has passed.
const windows: [string, ReloadDelay, number, number][] = [
	// Dials at 10, 50, and 90; the last runs past the window and gives up when it fails.
	[
		"an attempt under way finishes past the window",
		{ initial: Duration.millis(10), timeout: Duration.millis(100) },
		120,
		3,
	],
	// The first failure at 50 ms leaves 10 ms, less than the 20 ms wait, so it gives up at the deadline.
	[
		"a wait that reaches the deadline gives up there",
		{ initial: Duration.millis(20), timeout: Duration.millis(60) },
		60,
		1,
	],
	// Zero is a real bound, not "unlimited": the session's end gives up without a redial.
	["a zero window gives up when the session ends", { initial: Duration.millis(10), timeout: Duration.ZERO }, 0, 0],
];
for (const [name, delay, gaveUp, hung] of windows) {
	test(`retry window: ${name}`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		// Full delay every time, so the waits are exact.
		const random = spyOn(Math, "random").mockReturnValue(1);
		const warned = spyOn(console, "warn").mockImplementation(() => {});
		const url = new URL("https://relay.example/room");
		const plan = dialPlan((dial) => (dial === 0 ? "ok" : "hang"));
		const reload = new Reload({
			url,
			websocket: { enabled: false },
			timeout: Duration.millis(30),
			delay: { multiplier: 1, max: Duration.millis(1000), ...delay },
		});
		try {
			await outlive(clock, reload, plan);
			expect(await elapse(clock, () => reload.error.peek() !== undefined)).toBe(gaveUp);
			// The last failure: a hung attempt's timeout, or the session's own end.
			expect(reload.error.peek() instanceof ConnectTimeout).toBe(hung > 0);
			expect(plan.dials() - 1).toBe(hung);
			expect(plan.hungCloses()).toBe(hung);
		} finally {
			reload.close();
			plan.restore();
			warned.mockRestore();
			random.mockRestore();
			installed.close();
		}
	});
}

// The window is checked again when the backoff ends: a timer that fires late, as after the
// device slept, gives up instead of dialing past the window.
test("retry window: a backoff that fires past the window gives up", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const random = spyOn(Math, "random").mockReturnValue(1);
	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const url = new URL("https://relay.example/room");
	const plan = dialPlan((dial) => (dial === 0 ? "ok" : "hang"));
	const reload = new Reload({
		url,
		websocket: { enabled: false },
		timeout: Duration.millis(30),
		delay: {
			initial: Duration.millis(10),
			multiplier: 1,
			max: Duration.millis(1000),
			timeout: Duration.millis(100),
		},
	});
	try {
		// A healthy session ends at 0; the window closes at 100 ms.
		await outlive(clock, reload, plan);
		// The redial at 10 ms times out at 40 ms and schedules the next dial for 50 ms.
		await clock.advance(Duration.millis(40));
		for (let i = 0; i < 3; i++) await settle();
		expect(plan.dials()).toBe(2);
		expect(reload.error.peek()).toBeUndefined();
		clock.jump(Duration.millis(200));
		for (let i = 0; i < 3; i++) await settle();
		expect(reload.error.peek()).toBeInstanceOf(ConnectTimeout);
		expect(plan.dials()).toBe(2);
	} finally {
		reload.close();
		plan.restore();
		warned.mockRestore();
		random.mockRestore();
		installed.close();
	}
});

// Each session lives 100 ms, so the backoff between them shows how `healthy` scores its end and
// how `max` caps the delay, matching moq-tokio's `connect.backoff`.
const backoffs: [string, ReloadDelay, number[]][] = [
	// Past `initial` but short of the default bar, so each end is a failed attempt.
	["a session short of the default healthy bar escalates", {}, [10, 20, 40]],
	["a session past the healthy bar resets", { healthy: Duration.millis(50) }, [10, 10, 10]],
	[
		"a null max leaves the delay uncapped",
		{ initial: Duration.millis(1000), multiplier: 4, max: null },
		[1000, 4000, 16000],
	],
	["a max below initial caps the first delay too", { max: Duration.millis(5) }, [5, 5, 5]],
	// Zero is a real cap, not "uncapped".
	["a zero max retries at once", { max: Duration.ZERO }, [0, 0, 0]],
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
			delay: { initial: Duration.millis(10), multiplier: 2, max: Duration.millis(1000), ...delay },
		});
		try {
			const waited: number[] = [];
			for (let session = 0; session < gaps.length; session++) {
				await waitUntil(() => reload.status.peek() === "connected");
				await clock.advance(Duration.millis(100));
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
		delay: { initial: Duration.millis(10), multiplier: 1e300, max: null },
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
	const invalid: ReloadDelay[] = [
		// Zero would count every session as healthy.
		{ healthy: Duration.ZERO },
		{ multiplier: Number.NaN },
		{ multiplier: Number.POSITIVE_INFINITY },
	];
	for (const delay of invalid) {
		expect(() => new Reload({ url: new URL("https://example.com/"), delay }), String(Object.keys(delay))).toThrow(
			RangeError,
		);
	}
});

/**
 * An in-memory `WebSocketStream`, which qmux prefers when the platform has one, whose server
 * side `onServer` takes once the client's handshake is offered. The server picks the first
 * subprotocol the client offers.
 */
function linkedWebSocketStream(onServer: (socket: WebSocketStreamLike, protocol: string) => void) {
	return function StubWebSocketStream(url: string, options?: { protocols?: string[] }) {
		const protocol = options?.protocols?.[0] ?? "";
		const up = new TransformStream<Uint8Array | string, Uint8Array | string>();
		const down = new TransformStream<Uint8Array | string, Uint8Array | string>();
		const closed = Promise.withResolvers<{ closeCode: number; reason: string }>();
		const close = () => {
			closed.resolve({ closeCode: 1000, reason: "" });
			up.writable.abort().catch(() => {});
			down.writable.abort().catch(() => {});
		};
		onServer(
			{
				url,
				opened: Promise.resolve({ readable: up.readable, writable: down.writable, protocol, extensions: "" }),
				closed: closed.promise,
				close,
			},
			protocol,
		);
		return {
			url,
			opened: Promise.resolve({ readable: down.readable, writable: up.writable, protocol, extensions: "" }),
			closed: closed.promise,
			close,
		};
	};
}

// A link that fell back to WebSocket while WebTransport was unreachable probes it on a backoff
// and, once it answers, leaves the fallback make-before-break: the old session tells its peer it
// is moving with a GOAWAY and drains while the replacement finishes its handshake over the
// probe's WebTransport session, with no second race, and the status never reports a disconnect.
test("a WebSocket fallback returns to WebTransport once it is reachable", async () => {
	using clock = manual();
	const originalWebTransport = globalThis.WebTransport;
	const originalStream = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
	const url = new URL("https://fallback-returns.example.com/");

	let blocked = true;
	let probes = 0;
	const sessions: { transport: string; server: Promise<Session> }[] = [];
	globalThis.WebTransport = function StubWebTransport(dialed: string | URL) {
		if (blocked) {
			const refused = Promise.reject(new Error("UDP blocked"));
			refused.catch(() => {});
			return { ready: refused, closed: refused, close() {} };
		}
		const pair = createMockTransportPair(Lite.ALPN_06);
		const server = accept(pair.server, new URL(dialed));
		server.catch(() => {});
		sessions.push({ transport: "webtransport", server });
		probes++;
		return pair.client;
	} as unknown as typeof WebTransport;
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = linkedWebSocketStream((socket, protocol) => {
		const transport = Qmux.accept(socket, { protocol }) as unknown as WebTransport;
		// The negotiated protocol is known once the socket opens.
		sessions.push({ transport: "websocket", server: transport.ready.then(() => accept(transport, url)) });
	});

	const statuses: string[] = [];
	const reload = new Reload({ url, versions: [Lite.ALPN_06], delay: { initial: Duration.ZERO } });
	const watch = new Effect();
	watch.subscribe(reload.status, (status) => statuses.push(status));
	try {
		// The fallback joins the race after WebTransport's head start.
		for (let i = 0; i < 10 && reload.established.peek()?.transport !== "websocket"; i++) {
			await clock.advance(Duration.millis(100));
		}
		expect(reload.established.peek()?.transport).toBe("websocket");
		const fallback = sessions[0];
		if (!fallback) throw new Error("no fallback session");

		// Every probe fails while WebTransport is unreachable; the fallback keeps serving.
		await clock.advance(Duration.millis(70_000));
		expect(reload.established.peek()?.transport).toBe("websocket");

		blocked = false;
		// Past the fourth probe's longest wait after the third.
		for (let i = 0; i < 80 && reload.established.peek()?.transport !== "webtransport"; i++) {
			await clock.advance(Duration.millis(1_000));
		}
		expect(reload.established.peek()?.transport).toBe("webtransport");
		// The probe's session carries the replacement: one WebTransport dial, never another race.
		expect(probes).toBe(1);
		expect(sessions.filter((session) => session.transport === "websocket")).toHaveLength(1);
		expect(await (await fallback.server).draining).toEqual({ uri: "" });
		expect(statuses).not.toContain("disconnected");
	} finally {
		watch.close();
		reload.close();
		for (const session of sessions) (await session.server.catch(() => undefined))?.abort();
		globalThis.WebTransport = originalWebTransport;
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = originalStream;
	}
});

// A redirect the fallback's peer sends while it drains, after a probe got through but its
// handshake failed, still decides where the replacement goes: the redial after the backoff goes
// where the peer named, not back to the node that is leaving.
test("a redirect from the draining WebSocket fallback is followed", async () => {
	using clock = manual();
	const originalWebTransport = globalThis.WebTransport;
	const originalStream = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
	const url = new URL("https://fallback-redirect.example.com/");
	const elsewhere = new URL("https://fallback-redirect.example.com:4444/");

	let blocked = true;
	const dialed: string[] = [];
	const servers: Promise<Session>[] = [];
	let fallback: WebTransport | undefined;
	globalThis.WebTransport = function StubWebTransport(to: string | URL) {
		if (blocked) {
			const refused = Promise.reject(new Error("UDP blocked"));
			refused.catch(() => {});
			return { ready: refused, closed: refused, close() {} };
		}
		dialed.push(String(to));
		// The leaving node answers the probe, but no MoQ version is in common with it.
		if (new URL(to).port !== elsewhere.port) return createMockTransportPair("h3").client;
		const pair = createMockTransportPair(Lite.ALPN_06);
		const server = accept(pair.server, new URL(to));
		server.catch(() => {});
		servers.push(server);
		return pair.client;
	} as unknown as typeof WebTransport;
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = linkedWebSocketStream((socket, protocol) => {
		const transport = Qmux.accept(socket, { protocol }) as unknown as WebTransport;
		fallback = transport;
		servers.push(transport.ready.then(() => accept(transport, url)));
	});

	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const reload = new Reload({ url, versions: [Lite.ALPN_06], delay: { initial: Duration.millis(5_000) } });
	try {
		for (let i = 0; i < 10 && reload.established.peek()?.transport !== "websocket"; i++) {
			await clock.advance(Duration.millis(100));
		}
		const server = servers[0];
		if (!server || !fallback) throw new Error("no fallback session");

		blocked = false;
		// The probe gets through, the fallback tells its peer it is leaving, and the handshake
		// over the probe fails, so the loop backs off with the fallback still serving.
		for (let i = 0; i < 20 && dialed.length === 0; i++) await clock.advance(Duration.millis(1_000));
		expect(await (await server).draining).toEqual({ uri: "" });
		await clock.advance(Duration.millis(10));
		expect(reload.established.peek()?.transport).toBe("websocket");

		const stream = await Stream.open(fallback, { version: Lite.Version.DRAFT_06 });
		await stream.writer.u53(Lite.StreamId.Goaway);
		await new Lite.Goaway(elsewhere.href).encode(stream.writer, Lite.Version.DRAFT_06);
		for (let i = 0; i < 100 && reload.established.peek()?.transport !== "webtransport"; i++) {
			await clock.advance(Duration.millis(100));
		}
		expect(reload.established.peek()?.transport).toBe("webtransport");
		expect(dialed.map((to) => new URL(to).port)).toEqual(["", elsewhere.port]);
	} finally {
		reload.close();
		warned.mockRestore();
		for (const server of servers) (await server.catch(() => undefined))?.abort();
		globalThis.WebTransport = originalWebTransport;
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = originalStream;
	}
});

// A redirect the draining fallback's peer sends while the probe's replacement serves is kept:
// once that replacement closes and the fallback serves again, the redial goes where the peer
// named.
test("a redirect from the draining WebSocket fallback outlives a replacement that closes", async () => {
	using clock = manual();
	const originalWebTransport = globalThis.WebTransport;
	const originalStream = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
	const url = new URL("https://fallback-held.example.com/");
	const elsewhere = new URL("https://fallback-held.example.com:4444/");

	let blocked = true;
	const dialed: string[] = [];
	const replacements: Promise<Session>[] = [];
	const servers: Promise<Session>[] = [];
	let fallback: WebTransport | undefined;
	globalThis.WebTransport = function StubWebTransport(to: string | URL) {
		if (blocked) {
			const refused = Promise.reject(new Error("UDP blocked"));
			refused.catch(() => {});
			return { ready: refused, closed: refused, close() {} };
		}
		dialed.push(String(to));
		const pair = createMockTransportPair(Lite.ALPN_06);
		const server = accept(pair.server, new URL(to));
		server.catch(() => {});
		servers.push(server);
		replacements.push(server);
		return pair.client;
	} as unknown as typeof WebTransport;
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = linkedWebSocketStream((socket, protocol) => {
		const transport = Qmux.accept(socket, { protocol }) as unknown as WebTransport;
		fallback = transport;
		servers.push(transport.ready.then(() => accept(transport, url)));
	});

	const warned = spyOn(console, "warn").mockImplementation(() => {});
	const reload = new Reload({
		url,
		versions: [Lite.ALPN_06],
		delay: { initial: Duration.millis(1_000) },
		goaway: { handover: Duration.millis(60_000) },
	});
	try {
		for (let i = 0; i < 10 && reload.established.peek()?.transport !== "websocket"; i++) {
			await clock.advance(Duration.millis(100));
		}
		if (!fallback) throw new Error("no fallback session");

		blocked = false;
		for (let i = 0; i < 20 && reload.established.peek()?.transport !== "webtransport"; i++) {
			await clock.advance(Duration.millis(1_000));
		}
		expect(reload.established.peek()?.transport).toBe("webtransport");

		const stream = await Stream.open(fallback, { version: Lite.Version.DRAFT_06 });
		await stream.writer.u53(Lite.StreamId.Goaway);
		await new Lite.Goaway(elsewhere.href).encode(stream.writer, Lite.Version.DRAFT_06);
		await clock.advance(Duration.millis(10));

		// The replacement closes, so the fallback serves again until the redial.
		(await replacements[0])?.abort();
		for (let i = 0; i < 100 && dialed.length < 2; i++) await clock.advance(Duration.millis(100));
		expect(dialed.map((to) => new URL(to).port)).toEqual(["", elsewhere.port]);
	} finally {
		reload.close();
		warned.mockRestore();
		for (const server of servers) (await server.catch(() => undefined))?.abort();
		globalThis.WebTransport = originalWebTransport;
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = originalStream;
	}
});

// A WebSocket win is provisional: once the session it carried ends, the redial gives
// WebTransport its head start again, since the outage that lost the race may be over.
test("a WebSocket session ending restores WebTransport's head start", async () => {
	using clock = manual();
	const originalWebTransport = globalThis.WebTransport;
	const originalStream = (globalThis as { WebSocketStream?: unknown }).WebSocketStream;
	const url = new URL("https://fallback-ends.example.com/");

	const dials: Instant[] = [];
	const servers: Promise<Session>[] = [];
	globalThis.WebTransport = function BlockedWebTransport() {
		const refused = Promise.reject(new Error("UDP blocked"));
		refused.catch(() => {});
		return { ready: refused, closed: refused, close() {} };
	} as unknown as typeof WebTransport;
	(globalThis as { WebSocketStream?: unknown }).WebSocketStream = linkedWebSocketStream((socket, protocol) => {
		dials.push(now());
		const transport = Qmux.accept(socket, { protocol }) as unknown as WebTransport;
		servers.push(transport.ready.then(() => accept(transport, url)));
	});

	const reload = new Reload({ url, versions: [Lite.ALPN_06], delay: { initial: Duration.ZERO } });
	try {
		for (let i = 0; i < 10 && reload.established.peek()?.transport !== "websocket"; i++) {
			await clock.advance(Duration.millis(100));
		}
		const first = servers[0];
		if (!first) throw new Error("no fallback session");
		// Let the server's half of the handshake finish before ending it.
		await clock.advance(Duration.millis(100));
		(await first).abort();
		const ended = now();
		for (let i = 0; i < 20 && dials.length < 2; i++) {
			await clock.advance(Duration.millis(100));
		}
		const redial = dials[1];
		if (redial === undefined) throw new Error("never redialed the fallback");
		expect(redial.since(ended).ge(Duration.millis(500))).toBe(true);
	} finally {
		reload.close();
		for (const server of servers) (await server.catch(() => undefined))?.abort();
		globalThis.WebTransport = originalWebTransport;
		(globalThis as { WebSocketStream?: unknown }).WebSocketStream = originalStream;
	}
});
