import { expect, jest, test } from "bun:test";
import { SessionCode, SessionError } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import { ALPN, ALPN_05, ALPN_06, ALPN_07_WIP } from "../lite/version.ts";
import { createMockTransportPair } from "../mock.ts";
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
	jest.useFakeTimers();
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
		// Drain the finite async setup chain before advancing the mocked clock.
		for (let i = 0; i < 64; i++) await Promise.resolve();
		jest.advanceTimersByTime(1_000);
		for (let i = 0; i < 64; i++) await Promise.resolve();
		expect(localCloses).toBe(0);
		closed.resolve({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		const cause = await result;
		expect(cause).toBeInstanceOf(SessionError);
		expect((cause as SessionError).code).toBe(SessionCode.Unauthorized);
	} finally {
		closed.resolve({ closeCode: 0, reason: "test cleanup" });
		await result;
		jest.useRealTimers();
	}
});

test("versions map to WebTransport ALPNs in order", async () => {
	const versions = ["moq-lite-07-wip", "moq-lite-06", "moq-transport-22", "moq-lite-02", "moq-transport-14"];
	expect((await offered(ALPN_07_WIP, { versions })).protocols).toEqual([
		ALPN_07_WIP,
		ALPN_06,
		Ietf.ALPN.DRAFT_22,
		ALPN,
	]);
});

test("versions reject empty, duplicate, unsupported and ALPN spellings", async () => {
	for (const versions of [[], ["moq-lite-06", "moq-lite-06"], ["moq-lite-99"], ["toString"], [Ietf.ALPN.DRAFT_22]]) {
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

test("SETUP offers only the named versions to a peer without an ALPN", async () => {
	const pair = createMockTransportPair("");
	const restore = stubWebTransport(pair.client);
	try {
		const [client, server] = await Promise.all([
			connect(url, { versions: ["moq-lite-06", "moq-transport-14"], websocket: { enabled: false } }),
			accept({ url, transport: pair.server }),
		]);
		expect(client.version).toBe("moq-transport-14");
		client.abort();
		server.abort();
	} finally {
		restore();
	}
});

test("a peer without an ALPN is refused when no named version negotiates in SETUP", async () => {
	const pair = createMockTransportPair("");
	const restore = stubWebTransport(pair.client);
	try {
		await expect(connect(url, { versions: ["moq-lite-06"], websocket: { enabled: false } })).rejects.toThrow(
			"server negotiated no protocol",
		);
	} finally {
		restore();
	}
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
