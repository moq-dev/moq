import { expect, test } from "bun:test";
import { ALPN_05 } from "../lite/version.ts";
import { createMockTransportPair } from "../mock.ts";
import { connect } from "./connect.ts";

const url = new URL("https://example.com/test");

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

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
