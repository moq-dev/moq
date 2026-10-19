import { expect, jest, test } from "bun:test";
import { Manual, Nano } from "@moq/clock";
import { SessionCode, SessionError } from "../error.ts";
import * as Ietf from "../ietf/index.ts";
import { ALPN_05, ALPN_06, ALPN_07_WIP } from "../lite/version.ts";
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
	const { protocols, version } = await offered(ALPN_07_WIP, { webtransport: { protocols: [ALPN_07_WIP] } });
	expect(protocols).toEqual(["moq-lite-07-wip"]);
	expect(version).toBe("moq-lite-07-wip");
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

test("one protocol preference reaches WebTransport", async () => {
	const protocols = [ALPN_07_WIP, ALPN_06];
	expect((await offered(ALPN_07_WIP, { protocols })).protocols).toEqual(protocols);
});

test("one protocol preference reaches the WebSocket fallback", async () => {
	const originalTransport = globalThis.WebTransport;
	const originalSocket = globalThis.WebSocket;
	const clock = new Manual();
	const restore = clock.install();
	let protocols: string[] | undefined;
	globalThis.WebTransport = undefined as unknown as typeof WebTransport;
	globalThis.WebSocket = function CaptureSocket(_url: string, offered: string[]) {
		protocols = offered;
		throw new Error("captured WebSocket offer");
	} as unknown as typeof WebSocket;
	try {
		const result = connect(url, { protocols: [ALPN_07_WIP, ALPN_06] }).catch((err: unknown) => err);
		await clock.advanceAsync(Nano.zero);
		expect(await result).toBeInstanceOf(AggregateError);
		const application = protocols
			?.filter((protocol) => protocol.includes(".moq-"))
			.map((protocol) => protocol.slice(protocol.indexOf(".") + 1));
		// QMux may support more drafts later; every variant keeps the app preference.
		expect(application?.filter((protocol, index) => index === 0 || protocol !== application[index - 1])).toEqual([
			ALPN_07_WIP,
			ALPN_06,
		]);
	} finally {
		globalThis.WebTransport = originalTransport;
		globalThis.WebSocket = originalSocket;
		restore();
	}
});

test("protocol preferences reject empty, duplicate and unsupported offers", async () => {
	for (const protocols of [[], [ALPN_06, ALPN_06], ["unknown"]]) {
		expect(await connect(url, { protocols }).catch((err: unknown) => err)).toBeInstanceOf(TypeError);
	}
	expect(
		await connect(url, { protocols: [ALPN_06], webtransport: { protocols: [ALPN_05] } }).catch(
			(err: unknown) => err,
		),
	).toBeInstanceOf(TypeError);
});

test("a peer cannot select outside the explicit protocol offer", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const restore = stubWebTransport(pair.client);
	try {
		expect(
			await connect(url, { protocols: [ALPN_06], websocket: { enabled: false } }).catch((err: unknown) => err),
		).toHaveProperty("message", `server selected an unoffered protocol: ${ALPN_05}`);
	} finally {
		restore();
	}
});

test("IETF ALPN preferences are not confused with display names", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_22);
	const restore = stubWebTransport(pair.client);
	try {
		const [client, server] = await Promise.all([
			connect(url, { protocols: [Ietf.ALPN.DRAFT_22], websocket: { enabled: false } }),
			accept({ url, transport: pair.server }),
		]);
		expect(client.version).toBe("moq-transport-22");
		client.abort();
		server.abort();
	} finally {
		restore();
	}
});
