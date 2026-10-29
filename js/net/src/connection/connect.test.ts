import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano, sleep } from "@moq/clock";
import { ConnectTimeout, SessionCode, SessionError } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import { ClientSetup, ServerSetup } from "../lite/compat.ts";
import { ALPN, ALPN_05, ALPN_06, ALPN_07_WIP, Version as Lite } from "../lite/version.ts";
import { createMockTransportPair } from "../mock.ts";
import { Stream } from "../stream.ts";
import { accept } from "./accept.ts";
import { type ConnectProps, connect as connectSession } from "./connect.ts";

const url = new URL("https://example.com/test");

function connect(url: URL, props: Omit<ConnectProps, "url"> = {}) {
	return connectSession({ url, ...props });
}

// A relay URL as the token flow hands it to us.
const SECRET = "super-secret-jwt";
const authUrl = new URL(`https://example.com/test?jwt=${SECRET}#frag`);

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

const CONSOLE_METHODS = ["debug", "info", "log", "warn", "error"] as const;

// Collect everything connect writes to the console, joined per call.
function captureConsole(): { lines: string[]; restore: () => void } {
	const lines: string[] = [];
	const original = CONSOLE_METHODS.map((method) => [method, console[method]] as const);

	for (const method of CONSOLE_METHODS) {
		console[method] = (...args: unknown[]) => {
			lines.push(args.map((arg) => String(arg)).join(" "));
		};
	}

	return {
		lines,
		restore: () => {
			for (const [method, fn] of original) console[method] = fn;
		},
	};
}

// Hand connect() a mock transport through the normal `new WebTransport(...)` path, so
// the connect diagnostics run instead of being skipped by the `transport` option.
function stubWebTransport(transport: WebTransport): () => void {
	const original = globalThis.WebTransport;

	// Biome forbids returning a value from a class constructor.
	function StubWebTransport(this: unknown) {
		return transport;
	}
	globalThis.WebTransport = StubWebTransport as unknown as typeof WebTransport;

	return () => {
		globalThis.WebTransport = original;
	};
}

// Connect through a stubbed `new WebTransport(...)`, returning the offered protocols and
// the negotiated version.
async function offered(alpn: string, props: Omit<ConnectProps, "url"> = {}) {
	const pair = createMockTransportPair(alpn);
	const original = globalThis.WebTransport;
	let protocols: string[] | undefined;

	function StubWebTransport(this: unknown, _url: string | URL, options: WebTransportOptions) {
		protocols = options.protocols;
		return pair.client;
	}
	globalThis.WebTransport = StubWebTransport as unknown as typeof WebTransport;

	try {
		const connection = await connect(url, { websocket: { enabled: false }, ...props });
		connection.abort();
		return { protocols, version: connection.version };
	} finally {
		globalThis.WebTransport = original;
	}
}

test("WebTransport offers lite-06 first by default", async () => {
	const { protocols } = await offered(ALPN_06);
	expect(protocols?.[0]).toBe("moq-lite-06");
	expect(protocols).not.toContain(ALPN_07_WIP);
});

test("WebTransport negotiates lite-07-wip only when explicitly offered", async () => {
	const { protocols, version } = await offered(ALPN_07_WIP, { versions: ["moq-lite-07-wip"] });
	expect(protocols).toEqual(["moq-lite-07-wip"]);
	expect(version).toBe("moq-lite-07-wip");
});

test("a raw QUIC URL dials WebTransport over https", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const original = globalThis.WebTransport;
	let dialed: string | undefined;

	function StubWebTransport(this: unknown, target: string | URL) {
		dialed = target.toString();
		return pair.client;
	}
	globalThis.WebTransport = StubWebTransport as unknown as typeof WebTransport;

	try {
		const connection = await connect(new URL("moqt://example.com:4443/anon?jwt=x"), {
			websocket: { enabled: false },
		});
		connection.abort();
	} finally {
		globalThis.WebTransport = original;
	}

	expect(dialed).toBe("https://example.com:4443/anon?jwt=x");
});

test("a failed connect names why the transport failed", async () => {
	const original = globalThis.WebTransport;
	function FailingWebTransport(this: unknown): never {
		throw new Error("handshake refused");
	}
	globalThis.WebTransport = FailingWebTransport as unknown as typeof WebTransport;

	try {
		await expect(connect(url, { websocket: { enabled: false } })).rejects.toThrow("handshake refused");
	} finally {
		globalThis.WebTransport = original;
	}
});

test("connect logs the relay URL without its credentials", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const restoreTransport = stubWebTransport(pair.client);
	const captured = captureConsole();

	try {
		const connection = await connect(authUrl, { websocket: { enabled: false } });
		connection.abort();
	} finally {
		captured.restore();
		restoreTransport();
	}

	// The diagnostics must still identify the relay, just without the query.
	expect(captured.lines.length).toBeGreaterThan(0);
	expect(captured.lines.every((line) => line.includes("https://example.com/test"))).toBe(true);

	for (const line of captured.lines) {
		expect(line).not.toContain(SECRET);
		expect(line).not.toContain("jwt");
	}
});

test("already-aborted signal rejects without connecting", async () => {
	const original = globalThis.WebTransport;
	let connects = 0;

	class CountingWebTransport {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});

		constructor() {
			connects++;
		}

		close() {}
	}

	globalThis.WebTransport = CountingWebTransport as unknown as typeof WebTransport;

	try {
		const controller = new AbortController();
		controller.abort();

		const err = await connect(url, { signal: controller.signal, websocket: { enabled: false } }).then(
			() => undefined,
			(reason: unknown) => reason,
		);
		expect(err).toBeInstanceOf(DOMException);
		expect((err as DOMException).name).toBe("AbortError");
		expect(connects).toBe(0);
	} finally {
		globalThis.WebTransport = original;
	}
});

test("abort mid-connect rejects with the reason and closes the transport", async () => {
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

	try {
		const controller = new AbortController();
		const reason = new Error("deadline");

		const result = connect(url, { signal: controller.signal, websocket: { enabled: false } }).then(
			() => undefined,
			(err: unknown) => err,
		);

		await settle();
		expect(closes).toBe(0);

		controller.abort(reason);
		expect(await result).toBe(reason);

		await settle();
		expect(closes).toBe(1);
	} finally {
		globalThis.WebTransport = original;
	}
});

test("abort after a successful connect does nothing", async () => {
	const original = globalThis.WebTransport;
	const pair = createMockTransportPair(ALPN_05);

	// Biome forbids returning a value from a class constructor.
	function StubWebTransport(this: unknown) {
		return pair.client;
	}
	globalThis.WebTransport = StubWebTransport as unknown as typeof WebTransport;

	let closed = false;
	void pair.client.closed.then(() => {
		closed = true;
	});

	try {
		const controller = new AbortController();
		const connection = await connect(url, { signal: controller.signal, websocket: { enabled: false } });

		controller.abort();
		await settle();
		expect(closed).toBe(false);

		connection.abort();
	} finally {
		globalThis.WebTransport = original;
	}
});

test("abort race never returns a closed connection", async () => {
	// Sweep the microtask window between winning the transport race and returning its connection.
	for (let hops = 0; hops < 12; hops++) {
		const pair = createMockTransportPair(ALPN_05);
		const controller = new AbortController();
		const reason = new Error(`abort after ${hops} microtasks`);

		let transportClosed = false;
		void pair.client.closed.then(() => {
			transportClosed = true;
		});

		const outcome = connect(url, { signal: controller.signal, transport: pair.client }).then(
			(connection) => ({ connection }),
			(err: unknown) => ({ err }),
		);

		void (async () => {
			for (let i = 0; i < hops; i++) await Promise.resolve();
			controller.abort(reason);
		})();

		const result = await outcome;
		await settle();

		if ("connection" in result) {
			expect(transportClosed).toBe(false);
			result.connection.abort();
		} else {
			expect(result.err).toBe(reason);
			expect(transportClosed).toBe(true);
		}
	}
});

test("connect without a signal still works", async () => {
	const pair = createMockTransportPair(ALPN_05);

	const connection = await connect(url, { transport: pair.client });
	expect(connection.url.href).toBe(url.href);

	connection.abort();
});

// A session-level stream failure promises an authoritative session close, even when the
// browser publishes that close after the reset-only fallback would otherwise fire.
test("SETUP preserves a delayed session close code", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const closed = Promise.withResolvers<WebTransportCloseInfo>();
	let localCloses = 0;
	const transport = {
		protocol: "",
		closed: closed.promise,
		createBidirectionalStream: async () => {
			throw Object.assign(new Error("session closing"), { source: "session", streamErrorCode: null });
		},
		close: () => {
			localCloses++;
			closed.resolve({ closeCode: 0, reason: "" });
		},
	} as unknown as WebTransport;
	const result = connect(url, { transport }).then(
		() => new Error("unexpected admission"),
		(cause: unknown) => cause,
	);
	try {
		await clock.advance(Nano.fromMilli(Milli(1_000)));
		expect(localCloses).toBe(0);
		closed.resolve({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		const cause = await result;
		expect(cause).toBeInstanceOf(SessionError);
		expect((cause as SessionError).code).toBe(SessionCode.Unauthorized);
	} finally {
		closed.resolve({ closeCode: 0, reason: "test cleanup" });
		await result;
		installed.close();
	}
});

test("versions map to WebTransport ALPNs in order", async () => {
	const versions = ["moq-lite-07-wip", "moq-lite-06", "moq-transport-22", "moq-lite-02", "moq-lite-01"];
	expect((await offered(ALPN_07_WIP, { versions })).protocols).toEqual([
		ALPN_07_WIP,
		ALPN_06,
		Ietf.ALPN.DRAFT_22,
		ALPN,
	]);
});

test("versions reject empty, duplicate, unsupported and ALPN spellings", async () => {
	for (const versions of [
		[],
		["moq-lite-06", "moq-lite-06"],
		["moq-lite-99"],
		["moq-transport-14"],
		["moq-transport-17"],
		["toString"],
		[Ietf.ALPN.DRAFT_22],
	]) {
		expect(await connect(url, { versions }).catch((err: unknown) => err)).toBeInstanceOf(TypeError);
	}
});

test("the removed per-transport protocols fail loudly", async () => {
	for (const props of [{ webtransport: { protocols: [ALPN_06] } }, { websocket: { protocols: [ALPN_06] } }]) {
		await expect(connect(url, props as Omit<ConnectProps, "url">)).rejects.toThrow("protocols was removed");
	}
});

test("a peer cannot select outside the explicit versions", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const restore = stubWebTransport(pair.client);
	try {
		await expect(connect(url, { versions: ["moq-lite-06"], websocket: { enabled: false } })).rejects.toThrow(
			`server selected an unoffered protocol: ${ALPN_05}`,
		);
	} finally {
		restore();
	}
});

test("an IETF version name negotiates its ALPN", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_22);
	const restore = stubWebTransport(pair.client);
	try {
		const [client, server] = await Promise.all([
			connect(url, { versions: ["moq-transport-22"], websocket: { enabled: false } }),
			accept({ url, transport: pair.server }),
		]);
		expect(client.version).toBe("moq-transport-22");
		client.abort();
		server.abort();
	} finally {
		restore();
	}
});

// Firefox before 155 reports no subprotocol at all, so `protocol` is undefined rather than "".
for (const protocol of ["", undefined]) {
	test(`SETUP negotiates the named versions with a peer whose protocol is ${JSON.stringify(protocol)}`, async () => {
		for (const [versions, want] of [
			[["moq-lite-07-wip", "moq-lite-06"], "moq-lite-07-wip"],
			[["moq-lite-06", "moq-lite-02"], "moq-lite-06"],
			[["moq-lite-01", "moq-lite-06"], "moq-lite-01"],
			[["moq-lite-04"], "moq-lite-04"],
		] as const) {
			const pair = createMockTransportPair("");
			Object.defineProperty(pair.client, "protocol", { value: protocol });
			const restore = stubWebTransport(pair.client);
			try {
				const [client, server] = await Promise.all([
					connect(url, { versions, websocket: { enabled: false } }),
					accept({ url, transport: pair.server }),
				]);
				expect(client.version).toBe(want);
				expect(server.version).toBe(want);
				client.abort();
				server.abort();
			} finally {
				restore();
			}
		}
	});
}

test("SETUP refuses a version the client did not name", async () => {
	const pair = createMockTransportPair("");
	const restore = stubWebTransport(pair.client);
	try {
		const server = accept({ url, transport: pair.server, version: Lite.DRAFT_05 }).catch(() => undefined);
		await expect(
			connect(url, { versions: ["moq-lite-07-wip", "moq-lite-06"], websocket: { enabled: false } }),
		).rejects.toThrow("unsupported server version: 0xff0dad05");
		(await server)?.abort();
	} finally {
		restore();
	}
});

test("a peer without an ALPN is refused when no named version negotiates in SETUP", async () => {
	const pair = createMockTransportPair("");
	const restore = stubWebTransport(pair.client);
	try {
		await expect(connect(url, { versions: ["moq-transport-22"], websocket: { enabled: false } })).rejects.toThrow(
			"server negotiated no protocol",
		);
	} finally {
		restore();
	}
});

// The moq-transport draft-14 version code, which the `moql` SETUP once also negotiated.
const DRAFT_14 = 0xff00000e;

test("a client refuses an IETF version selected in the moql SETUP", async () => {
	const pair = createMockTransportPair("");
	const restore = stubWebTransport(pair.client);
	try {
		const connecting = connect(url, { websocket: { enabled: false } });
		const stream = await Stream.accept(pair.server, Lite.DRAFT_02);
		if (!stream) throw new Error("no SETUP stream");
		await ClientSetup.decode(stream.reader);
		await new ServerSetup(DRAFT_14).encode(stream.writer);
		await expect(connecting).rejects.toThrow("unsupported server version: 0xff00000e");
	} finally {
		restore();
	}
});

test("a server refuses a client offering only IETF versions in the moql SETUP", async () => {
	const pair = createMockTransportPair("");
	const accepting = accept({ url, transport: pair.server });
	// Handled now, since the refusal can land while the write below yields.
	accepting.catch(() => {});
	const stream = await Stream.open(pair.client, { version: Lite.DRAFT_02 });
	await new ClientSetup([DRAFT_14]).encode(stream.writer);
	await expect(accepting).rejects.toThrow("no common version found; client offered: ff00000e");
});

// The WebSocket subprotocols one connect() attempt asks a server for.
async function websocketOffer(props: Omit<ConnectProps, "url">): Promise<string[]> {
	const original = globalThis.WebTransport;
	// No WebTransport, so the WebSocket attempt goes at once.
	globalThis.WebTransport = undefined as unknown as typeof WebTransport;
	let offer: string[] = [];
	const server = Bun.serve({
		port: 0,
		fetch(request) {
			offer = (request.headers.get("sec-websocket-protocol") ?? "").split(",").map((protocol) => protocol.trim());
			return new Response("no", { status: 400 });
		},
	});
	const controller = new AbortController();
	try {
		const attempt = connect(new URL(`http://127.0.0.1:${server.port}/`), {
			signal: controller.signal,
			...props,
		}).catch(() => undefined);
		for (let i = 0; i < 100 && offer.length === 0; i++) await settle();
		controller.abort();
		await attempt;
		return offer;
	} finally {
		globalThis.WebTransport = original;
		server.stop(true);
	}
}

test("the WebSocket fallback offers the same versions", async () => {
	const defaults = await websocketOffer({});
	expect(defaults.some((protocol) => protocol.endsWith(ALPN_06))).toBe(true);
	expect(defaults.some((protocol) => protocol.endsWith(ALPN_07_WIP))).toBe(false);

	const asked = await websocketOffer({ versions: ["moq-lite-07-wip", "moq-lite-06"] });
	expect(asked[0]?.endsWith(ALPN_07_WIP)).toBe(true);
	expect(asked.some((protocol) => protocol.endsWith(ALPN_05))).toBe(false);
});

// A WebTransport whose handshake nothing ever answers, as a dial to a dead target is.
function silentWebTransport(): { closes: () => number; restore: () => void } {
	const original = globalThis.WebTransport;
	let closes = 0;
	class SilentWebTransport {
		ready = new Promise<void>(() => {});
		closed = new Promise<void>(() => {});
		close() {
			closes++;
		}
	}
	globalThis.WebTransport = SilentWebTransport as unknown as typeof WebTransport;
	return { closes: () => closes, restore: () => (globalThis.WebTransport = original) };
}

// Record how a connect settles, without awaiting it.
function track(attempt: Promise<unknown>): { outcome: () => unknown } {
	let outcome: unknown;
	attempt.then(
		() => (outcome = "connected"),
		(err: unknown) => (outcome = err),
	);
	return { outcome: () => outcome };
}

test("a dial nothing answers fails with ConnectTimeout after the default timeout and closes the transport", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const silent = silentWebTransport();
	try {
		const attempt = track(connect(url, { websocket: { enabled: false } }));
		await clock.advance(Nano.fromMilli(Milli(29_999)));
		expect(attempt.outcome()).toBeUndefined();

		await clock.advance(Nano.fromMilli(Milli(1)));
		await settle();
		expect(attempt.outcome()).toBeInstanceOf(ConnectTimeout);
		expect((attempt.outcome() as ConnectTimeout).timeout).toBe(Milli(30_000));
		// The dial and its hedge.
		expect(silent.closes()).toBe(2);
	} finally {
		silent.restore();
		installed.close();
	}
});

test("a timeout of 0 waits for the caller", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const silent = silentWebTransport();
	try {
		const controller = new AbortController();
		const attempt = track(
			connect(url, { websocket: { enabled: false }, timeout: Milli(0), signal: controller.signal }),
		);
		await clock.advance(Nano.fromMilli(Milli(600_000)));
		expect(attempt.outcome()).toBeUndefined();

		const reason = new Error("teardown");
		controller.abort(reason);
		await settle();
		expect(attempt.outcome()).toBe(reason);
	} finally {
		silent.restore();
		installed.close();
	}
});

test("the caller's abort wins over the timeout and disarms it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const silent = silentWebTransport();
	try {
		const controller = new AbortController();
		const attempt = track(connect(url, { websocket: { enabled: false }, signal: controller.signal }));
		await clock.advance(Nano.fromMilli(Milli(10_000)));
		const reason = new Error("teardown");
		controller.abort(reason);
		await settle();
		expect(attempt.outcome()).toBe(reason);

		await clock.advance(Nano.fromMilli(Milli(60_000)));
		expect(attempt.outcome()).toBe(reason);
		// The dial and its hedge.
		expect(silent.closes()).toBe(2);
	} finally {
		silent.restore();
		installed.close();
	}
});

test("a handshake the peer never answers times out on a supplied transport too", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		// The `moql` ALPN negotiates in SETUP, which nothing on the other side answers.
		const pair = createMockTransportPair(ALPN);
		let transportClosed = false;
		void pair.client.closed.then(
			() => (transportClosed = true),
			() => (transportClosed = true),
		);

		const attempt = track(connect(url, { transport: pair.client, timeout: Milli(5_000) }));
		await clock.advance(Nano.fromMilli(Milli(4_999)));
		expect(attempt.outcome()).toBeUndefined();

		await clock.advance(Nano.fromMilli(Milli(1)));
		await settle();
		expect(attempt.outcome()).toBeInstanceOf(ConnectTimeout);
		expect(transportClosed).toBe(true);
	} finally {
		installed.close();
	}
});

type Dial = "hang" | "ok" | { failAfter: Milli } | { openWhen: Promise<void> };

// Stub WebTransport so each dial behaves as `plan` says, recording when it was dialed (on
// the Manual clock) and how often it was closed.
function webTransportPlan(clock: Manual, plan: (dial: number) => Dial) {
	const original = globalThis.WebTransport;
	const dials: { at: number; closes: number }[] = [];
	globalThis.WebTransport = function StubWebTransport() {
		const index = dials.length;
		const record = { at: Nano.toMilli(Nano(clock.now())), closes: 0 };
		dials.push(record);
		const dial = plan(index);
		if (dial === "hang" || (typeof dial === "object" && "failAfter" in dial)) {
			const ready =
				dial === "hang"
					? new Promise<void>(() => {})
					: sleep(Nano.fromMilli(dial.failAfter)).then(() => {
							throw new Error(`refused ${index}`);
						});
			ready.catch(() => {});
			return {
				ready,
				closed: new Promise<void>(() => {}),
				close() {
					record.closes++;
				},
			};
		}
		const pair = createMockTransportPair(ALPN_06);
		const close = pair.client.close.bind(pair.client);
		pair.client.close = (info?: WebTransportCloseInfo) => {
			record.closes++;
			close(info);
		};
		if (dial !== "ok") Object.defineProperty(pair.client, "ready", { value: dial.openWhen.then(() => undefined) });
		return pair.client;
	} as unknown as typeof WebTransport;
	return { dials, restore: () => (globalThis.WebTransport = original) };
}

test("a dial still unopened after 500 ms is hedged, and the hedge wins when the first hangs", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webTransportPlan(clock, (dial) => (dial === 0 ? "hang" : "ok"));
	try {
		const attempt = connect(url, { websocket: { enabled: false } });
		await clock.advance(Nano.fromMilli(Milli(499)));
		expect(plan.dials.length).toBe(1);

		await clock.advance(Nano.fromMilli(Milli(1)));
		const connection = await attempt;
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 500]);
		await settle();
		expect(plan.dials.map((dial) => dial.closes)).toEqual([1, 0]);
		connection.abort();
	} finally {
		plan.restore();
		installed.close();
	}
});

test("a dial that opens within 500 ms is not hedged", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webTransportPlan(clock, () => "ok");
	try {
		const connection = await connect(url, { websocket: { enabled: false } });
		await clock.advance(Nano.fromMilli(Milli(5_000)));
		expect(plan.dials.length).toBe(1);
		connection.abort();
	} finally {
		plan.restore();
		installed.close();
	}
});

test("a dial that fails outright is not hedged", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webTransportPlan(clock, () => ({ failAfter: Milli(100) }));
	try {
		const attempt = track(connect(url, { websocket: { enabled: false } }));
		await clock.advance(Nano.fromMilli(Milli(5_000)));
		await settle();
		expect(plan.dials.length).toBe(1);
		expect((attempt.outcome() as Error).message).toBe("no transport connected: refused 0");
	} finally {
		plan.restore();
		installed.close();
	}
});

test("when a dial and its hedge both fail, the first dial's reason is reported", async () => {
	const clock = new Manual();
	const installed = clock.install();
	// The hedge dials at 500 ms and fails at 600 ms; the first fails at 1.5 s.
	const plan = webTransportPlan(clock, (dial) => ({ failAfter: Milli(dial === 0 ? 1_500 : 100) }));
	try {
		const attempt = track(connect(url, { websocket: { enabled: false } }));
		await clock.advance(Nano.fromMilli(Milli(2_000)));
		await settle();
		expect(plan.dials.length).toBe(2);
		expect((attempt.outcome() as Error).message).toBe("no transport connected: refused 0");
	} finally {
		plan.restore();
		installed.close();
	}
});

test("when a dial and its hedge open in the same turn, one wins and the other is closed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	let open = () => {};
	const opened = new Promise<void>((resolve) => (open = resolve));
	// The first opens the moment the hedge dials, and the hedge opens at once.
	const plan = webTransportPlan(clock, (dial) => {
		if (dial === 0) return { openWhen: opened };
		open();
		return "ok";
	});
	try {
		const attempt = connect(url, { websocket: { enabled: false } });
		await clock.advance(Nano.fromMilli(Milli(500)));
		const connection = await attempt;
		await settle();
		expect(plan.dials.length).toBe(2);
		expect(plan.dials.map((dial) => dial.closes).sort()).toEqual([0, 1]);
		connection.abort();
	} finally {
		plan.restore();
		installed.close();
	}
});

test("a hedge that opens after the first dial failed still wins", async () => {
	const clock = new Manual();
	const installed = clock.install();
	// The hedge dials at 500 ms and opens at 2 s; the first fails at 1.5 s in between.
	const plan = webTransportPlan(clock, (dial) =>
		dial === 0 ? { failAfter: Milli(1_500) } : { openWhen: sleep(Nano.fromMilli(Milli(1_500))) },
	);
	try {
		const attempt = connect(url, { websocket: { enabled: false } });
		await clock.advance(Nano.fromMilli(Milli(2_000)));
		const connection = await attempt;
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 500]);
		connection.abort();
	} finally {
		plan.restore();
		installed.close();
	}
});

test("when the WebSocket wins, every WebTransport dial is closed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webTransportPlan(clock, () => "hang");
	// The WebSocket dials after its 500 ms head start, with the hedge, and opens 100 ms later.
	const native = globalThis as { WebSocketStream?: unknown };
	const originalStream = native.WebSocketStream;
	native.WebSocketStream = class {
		url: string;
		opened = sleep(Nano.fromMilli(Milli(100))).then(() => ({
			readable: new ReadableStream(),
			writable: new WritableStream(),
			protocol: "qmux-01",
			extensions: "",
		}));
		closed = new Promise(() => {});
		constructor(url: string) {
			this.url = url;
		}
		close() {}
	};
	const controller = new AbortController();
	try {
		const pending = connect(new URL("https://example.com/websocket-wins"), { signal: controller.signal });
		await clock.advance(Nano.fromMilli(Milli(700)));
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 500]);
		expect(plan.dials.map((dial) => dial.closes)).toEqual([1, 1]);

		// The WebSocket's SETUP is never answered.
		controller.abort();
		await pending.catch(() => undefined);
	} finally {
		native.WebSocketStream = originalStream;
		plan.restore();
		installed.close();
	}
});

test("when WebTransport and WebSocket open in the same turn, the one not chosen is closed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	let open = () => {};
	const opened = new Promise<void>((resolve) => (open = resolve));
	const plan = webTransportPlan(clock, () => ({ openWhen: opened }));
	// A native-shaped WebSocketStream that opens on the same signal.
	let websocketCloses = 0;
	const native = globalThis as { WebSocketStream?: unknown };
	const originalStream = native.WebSocketStream;
	native.WebSocketStream = class {
		url: string;
		opened = opened.then(() => ({
			readable: new ReadableStream(),
			writable: new WritableStream(),
			protocol: "qmux-01",
			extensions: "",
		}));
		closed = new Promise(() => {});
		constructor(url: string) {
			this.url = url;
		}
		close() {
			websocketCloses++;
		}
	};
	const controller = new AbortController();
	try {
		// Its own URL: a WebSocket win drops WebTransport's head start for that URL afterwards.
		const pending = connect(new URL("https://example.com/same-turn"), {
			websocket: { delay: Milli(0) },
			signal: controller.signal,
		});
		await settle();
		open();
		await settle();
		await settle();
		// Exactly one transport lost the race, and it was closed.
		expect(plan.dials[0].closes + websocketCloses).toBe(1);

		// A WebSocket winner is still in SETUP, which nothing answers; a WebTransport one connected.
		controller.abort();
		const connection = await pending.catch(() => undefined);
		connection?.abort();
	} finally {
		native.WebSocketStream = originalStream;
		plan.restore();
		installed.close();
	}
});

test("the caller's abort wins over a timeout that fires in the same turn", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const silent = silentWebTransport();
	try {
		const controller = new AbortController();
		const attempt = track(connect(url, { websocket: { enabled: false }, signal: controller.signal }));
		await settle();
		const reason = new Error("teardown");
		// Fire the timeout without running any task, then abort before one runs.
		clock.jump(Nano.fromMilli(Milli(30_000)));
		controller.abort(reason);
		await settle();
		expect(attempt.outcome()).toBe(reason);
	} finally {
		silent.restore();
		installed.close();
	}
});

test("a connected session outlives its connect timeout", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const pair = createMockTransportPair(ALPN_06);
		let closed = false;
		void pair.client.closed.then(() => (closed = true));
		const connection = await connect(url, { transport: pair.client, timeout: Milli(1_000) });
		await clock.advance(Nano.fromMilli(Milli(2_000)));
		await settle();
		expect(closed).toBe(false);
		connection.abort();
	} finally {
		installed.close();
	}
});

test("timeout race never returns a closed connection", async () => {
	// Sweep the microtask window between winning the transport race and returning its
	// connection, as "abort race never returns a closed connection" does for the signal.
	for (let hops = 0; hops < 12; hops++) {
		const clock = new Manual();
		const installed = clock.install();
		try {
			const pair = createMockTransportPair(ALPN_05);
			let transportClosed = false;
			void pair.client.closed.then(() => {
				transportClosed = true;
			});
			const outcome = connect(url, { transport: pair.client, timeout: Milli(1) }).then(
				(connection) => ({ connection }),
				(err: unknown) => ({ err }),
			);
			void (async () => {
				for (let i = 0; i < hops; i++) await Promise.resolve();
				clock.jump(Nano.fromMilli(Milli(1)));
			})();
			const result = await outcome;
			await settle();
			if ("connection" in result) {
				expect(transportClosed, `hops ${hops}`).toBe(false);
				result.connection.abort();
			} else {
				expect(result.err, `hops ${hops}`).toBeInstanceOf(ConnectTimeout);
				expect(transportClosed, `hops ${hops}`).toBe(true);
			}
		} finally {
			installed.close();
		}
	}
});

test("an invalid timeout throws before touching the caller's signal", async () => {
	const controller = new AbortController();
	const added = spyOn(controller.signal, "addEventListener");
	for (const timeout of [-1, Number.NaN, Number.POSITIVE_INFINITY]) {
		await expect(
			connect(url, { websocket: { enabled: false }, timeout: timeout as Milli, signal: controller.signal }),
		).rejects.toThrow(RangeError);
	}
	expect(added).not.toHaveBeenCalled();
	// In the caller's unit, not the nanoseconds it is converted to.
	await expect(connect(url, { timeout: -1 as Milli })).rejects.toThrow("invalid time: -1");
});

type WebSocketDial = "hang" | "open" | { failAfter: Milli } | { openAfter: Milli };

// Their own URL: a WebSocket that wins drops WebTransport's head start for its URL afterwards.
const websocketUrl = new URL("https://example.com/websocket-only");

// Stub a native WebSocketStream so each dial behaves as `plan` says, recording when it was
// dialed (on the Manual clock) and how often it was closed. WebTransport is removed, as in
// Safari, so the WebSocket is the only transport and dials at once.
function webSocketPlan(clock: Manual, plan: (dial: number) => WebSocketDial) {
	const native = globalThis as { WebSocketStream?: unknown; WebTransport?: unknown };
	const originalStream = native.WebSocketStream;
	const originalTransport = native.WebTransport;
	native.WebTransport = undefined;
	const dials: { at: number; closes: number }[] = [];
	const streams = () => ({
		readable: new ReadableStream(),
		writable: new WritableStream(),
		protocol: "qmux-01",
		extensions: "",
	});
	native.WebSocketStream = class {
		url: string;
		opened: Promise<unknown>;
		closed = new Promise(() => {});
		#record: { at: number; closes: number };
		constructor(url: string) {
			this.url = url;
			const index = dials.length;
			this.#record = { at: Nano.toMilli(Nano(clock.now())), closes: 0 };
			dials.push(this.#record);
			const dial = plan(index);
			if (dial === "hang") this.opened = new Promise(() => {});
			else if (dial === "open") this.opened = Promise.resolve(streams());
			else if ("failAfter" in dial)
				this.opened = sleep(Nano.fromMilli(dial.failAfter)).then(() => {
					throw new Error(`refused ${index}`);
				});
			else this.opened = sleep(Nano.fromMilli(dial.openAfter)).then(streams);
			this.opened.catch(() => {});
		}
		close() {
			this.#record.closes++;
		}
	};
	return {
		dials,
		restore: () => {
			native.WebSocketStream = originalStream;
			native.WebTransport = originalTransport;
		},
	};
}

test("a WebSocket dial still unopened after 10 s is closed and dialed again", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webSocketPlan(clock, (dial) => (dial === 0 ? "hang" : "open"));
	const controller = new AbortController();
	try {
		const pending = connect(websocketUrl, { signal: controller.signal });
		await clock.advance(Nano.fromMilli(Milli(9_999)));
		expect(plan.dials.length).toBe(1);

		await clock.advance(Nano.fromMilli(Milli(1)));
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 10_000]);
		// The session flushes its close for 100 ms before closing the socket.
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(plan.dials.map((dial) => dial.closes)).toEqual([1, 0]);

		// The second dial opened; its SETUP is never answered.
		controller.abort();
		await pending.catch(() => undefined);
	} finally {
		plan.restore();
		installed.close();
	}
});

test("each WebSocket redial within an attempt waits twice as long", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webSocketPlan(clock, () => "hang");
	const controller = new AbortController();
	try {
		const pending = connect(websocketUrl, { timeout: Milli(0), signal: controller.signal });
		await clock.advance(Nano.fromMilli(Milli(75_000)));
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 10_000, 30_000, 70_000]);
		expect(plan.dials.map((dial) => dial.closes)).toEqual([1, 1, 1, 0]);

		controller.abort();
		await pending.catch(() => undefined);
	} finally {
		plan.restore();
		installed.close();
	}
});

test("a WebSocket dial that fails outright is not redialed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webSocketPlan(clock, () => ({ failAfter: Milli(100) }));
	try {
		const attempt = track(connect(websocketUrl, {}));
		await clock.advance(Nano.fromMilli(Milli(20_000)));
		await settle();
		expect(plan.dials.length).toBe(1);
		expect((attempt.outcome() as Error).message).toBe("no transport connected: refused 0");
	} finally {
		plan.restore();
		installed.close();
	}
});

test("a WebSocket dial that opens within 10 s is not redialed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webSocketPlan(clock, () => ({ openAfter: Milli(5_000) }));
	const controller = new AbortController();
	try {
		const pending = connect(websocketUrl, { signal: controller.signal });
		await clock.advance(Nano.fromMilli(Milli(15_000)));
		expect(plan.dials.length).toBe(1);

		controller.abort();
		await pending.catch(() => undefined);
	} finally {
		plan.restore();
		installed.close();
	}
});

test("with the default timeout, a silent WebSocket is redialed once and the attempt then times out", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const plan = webSocketPlan(clock, () => "hang");
	try {
		const attempt = track(connect(websocketUrl, {}));
		await clock.advance(Nano.fromMilli(Milli(30_100)));
		await settle();
		expect(attempt.outcome()).toBeInstanceOf(ConnectTimeout);
		expect(plan.dials.map((dial) => dial.at)).toEqual([0, 10_000]);
		expect(plan.dials.map((dial) => dial.closes)).toEqual([1, 1]);
	} finally {
		plan.restore();
		installed.close();
	}
});
