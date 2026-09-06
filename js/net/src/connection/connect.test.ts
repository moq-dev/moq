import { expect, test } from "bun:test";
import { ALPN_05 } from "../lite/version.ts";
import { createMockTransportPair } from "../mock.ts";
import { connect } from "./connect.ts";

const url = new URL("https://example.com/test");

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

test("connect logs the relay URL without its credentials", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const restoreTransport = stubWebTransport(pair.client);
	const captured = captureConsole();

	const mode = process.env.MODE;
	const nodeEnv = process.env.NODE_ENV;
	process.env.MODE = "development";
	process.env.NODE_ENV = "development";

	try {
		const connection = await connect(authUrl, { websocket: { enabled: false } });
		connection.close();
	} finally {
		captured.restore();
		restoreTransport();
		if (mode === undefined) delete process.env.MODE;
		else process.env.MODE = mode;
		if (nodeEnv === undefined) delete process.env.NODE_ENV;
		else process.env.NODE_ENV = nodeEnv;
	}

	// The diagnostics must still identify the relay, just without the query.
	expect(captured.lines.length).toBeGreaterThan(0);
	expect(captured.lines.every((line) => line.includes("https://example.com/test"))).toBe(true);

	for (const line of captured.lines) {
		expect(line).not.toContain(SECRET);
		expect(line).not.toContain("jwt");
	}
});

test("connect emits no diagnostics in a production build", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const restoreTransport = stubWebTransport(pair.client);

	// Bun aliases `import.meta.env` to `process.env`, which is what the DEV check reads.
	const mode = process.env.MODE;
	process.env.MODE = "production";

	const captured = captureConsole();

	try {
		const connection = await connect(authUrl, { websocket: { enabled: false } });
		connection.close();
	} finally {
		captured.restore();
		restoreTransport();
		if (mode === undefined) {
			delete process.env.MODE;
		} else {
			process.env.MODE = mode;
		}
	}

	expect(captured.lines).toEqual([]);
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

		connection.close();
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
			result.connection.close();
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

	connection.close();
});
