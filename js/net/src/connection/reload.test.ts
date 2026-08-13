import { expect, test } from "bun:test";
import { Effect } from "@moq/signals";
import type { Producer as BroadcastProducer } from "../broadcast.ts";
import { RemoteError, SessionCode } from "../error.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
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

test("an explicitly undefined delay field falls back to its default", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	let dials = 0;
	const stub = function StubWebTransport() {
		dials += 1;
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		void accept(pair.server, url).then((server) => server.close());
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
		delay: { initial: undefined, multiplier: undefined, max: undefined, timeout: 0 },
	});
	try {
		await waitUntil(() => dials > 0);
		await new Promise((resolve) => setTimeout(resolve, 100));
		// The default initial delay is 1000ms, so the first retry is still pending.
		expect(dials).toBe(1);
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
		const origin = new OriginProducer();
		void accept(pair.server, url, { publish: origin.consume() }).then((server) => {
			sessions.push(server);
			published.push(origin.publish(Path.from("late")));
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

test("a reload that gives up stops claiming it will answer requests", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		void accept(pair.server, url).then(() => {
			pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const origin = new OriginProducer();
	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: 1, multiplier: 2, max: 1, timeout: 0 },
		subscribe: origin,
	});

	// A reconnecting connection holds requests pending, which is the point: no session is
	// attached yet and one is coming.
	const request = origin.consume().request(Path.from("wanted"));
	expect(request.unroutable.peek()).toBe(false);

	try {
		// Terminal: these credentials will never work, so nothing is coming after all and a
		// request must stop waiting on it rather than hanging on a connection that is done.
		await reload.closed.then(
			() => undefined,
			() => undefined,
		);
		await waitUntil(() => request.unroutable.peek() === true);
		expect(request.unroutable.peek()).toBe(true);
	} finally {
		request.close();
		reload.close();
		origin.close();
		globalThis.WebTransport = original;
	}
});

test("a session rejected as unauthorized surfaces the code and stops retrying", async () => {
	const original = globalThis.WebTransport;
	const url = new URL("https://example.com/");
	const closes: (Error | null)[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_06_WIP);
		// Reject at the MoQ layer: accept the transport, then close with a code, the
		// way a relay's Request::close does after it has already accepted the transport.
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
		delay: { initial: 1, multiplier: 2, max: 1, timeout: 0 },
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
		// UNAUTHORIZED is specified rather than guessed at, the loop treats it as terminal
		// instead of retrying credentials that cannot work. `timeout: 0` means unlimited
		// retries, so `closed` settling at all is what proves it stopped on the rejection.
		const err = await reload.closed.then(
			() => undefined,
			(err: unknown) => err,
		);
		expect(err).toBeInstanceOf(RemoteError);
		expect((err as RemoteError).code).toBe(SessionCode.Unauthorized);

		// It still surfaced through the established session before the loop gave up.
		expect(closes.find((e) => e instanceof RemoteError)).toBeInstanceOf(RemoteError);
	} finally {
		watch.close();
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
	publishOrigin.publish(Path.from("mine"));

	// Each connect attempt gets a fresh server session that publishes "remote" and records
	// what the client announced to it.
	const servers: { session: { close: () => void }; saw: OriginProducer }[] = [];
	const stub = function StubWebTransport() {
		const pair = createMockTransportPair(Lite.ALPN_05);
		const saw = new OriginProducer();
		const serverOrigin = new OriginProducer();
		void accept(pair.server, url, { publish: serverOrigin.consume(), subscribe: saw }).then((session) => {
			serverOrigin.publish(Path.from("remote"));
			servers.push({ session, saw });
		});
		return pair.client;
	};
	globalThis.WebTransport = stub as unknown as typeof WebTransport;

	const reload = new Reload({
		enabled: true,
		url,
		websocket: { enabled: false },
		delay: { initial: 10, multiplier: 1, max: 10 },
		publish: publishOrigin.consume(),
		subscribe: subscribeOrigin,
	});
	const reader = subscribeOrigin.consume();

	try {
		// First session: the server's broadcast lands in the client origin, and the client's
		// publish lands in the server's.
		await waitUntil(() => reader.routes(Path.from("remote")));
		await waitUntil(() => servers[0]?.saw.routes(Path.from("mine")));

		// Kill the session: the remote entry retracts, the local publish stays put.
		servers[0]?.session.close();
		await waitUntil(() => !reader.routes(Path.from("remote")));

		// The reconnect re-announces the (untouched) publish and re-populates the table.
		await waitUntil(() => servers.length > 1);
		await waitUntil(() => reader.routes(Path.from("remote")));
		await waitUntil(() => servers[1]?.saw.routes(Path.from("mine")));
	} finally {
		reload.close();
		publishOrigin.close();
		subscribeOrigin.close();
		globalThis.WebTransport = original;
	}
});
