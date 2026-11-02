import { describe, expect, test } from "bun:test";
import { Instant, Milli, Nano, now, yieldNow } from "@moq/clock";
import {
	openWebSocketStream,
	type WebSocketConstructor,
	type WebSocketLike,
	WebSocketStream,
} from "./web-socket-stream.ts";

/** A scriptable WebSocket stand-in implementing the bits the ponyfill uses. */
class FakeWebSocket implements WebSocketLike {
	static last: FakeWebSocket | undefined;

	binaryType = "blob";
	bufferedAmount = 0;
	readyState = 0; // CONNECTING
	protocol: string;
	extensions = "";
	sent: Array<string | ArrayBufferLike | ArrayBufferView> = [];
	closeArgs: { code?: number; reason?: string } | undefined;

	onopen: ((ev: unknown) => void) | null = null;
	onmessage: ((ev: { data: unknown }) => void) | null = null;
	onerror: ((ev: unknown) => void) | null = null;
	onclose: ((ev: { code?: number; reason?: string }) => void) | null = null;

	readonly url: string;

	constructor(url: string, protocols?: string | string[]) {
		this.url = url;
		this.protocol = Array.isArray(protocols) ? (protocols[0] ?? "") : (protocols ?? "");
		FakeWebSocket.last = this;
	}

	send(data: string | ArrayBufferLike | ArrayBufferView): void {
		this.sent.push(data);
	}
	close(code?: number, reason?: string): void {
		this.closeArgs = { code, reason };
		this.readyState = 3; // CLOSED
	}

	// Test helpers
	open() {
		this.readyState = 1; // OPEN
		this.onopen?.({});
	}
	message(data: unknown) {
		this.onmessage?.({ data });
	}
	error() {
		this.onerror?.({});
	}
	fireClose(code = 1000, reason = "") {
		this.readyState = 3;
		this.onclose?.({ code, reason });
	}
}

const ctor = FakeWebSocket as unknown as WebSocketConstructor;

/** Yield to the event loop `n` times to let queued work run. */
async function tick(n = 1): Promise<void> {
	for (let i = 0; i < n; i++) {
		await yieldNow();
	}
}

describe("WebSocketStream ponyfill", () => {
	test("opened resolves with streams and negotiated protocol", async () => {
		const wss = new WebSocketStream("wss://x/y", { webSocket: ctor, protocols: ["p1"] });
		const ws = FakeWebSocket.last as FakeWebSocket;
		expect(ws.binaryType).toBe("arraybuffer");
		ws.open();
		const { readable, writable, protocol } = await wss.opened;
		expect(protocol).toBe("p1");
		expect(readable).toBeInstanceOf(ReadableStream);
		expect(writable).toBeInstanceOf(WritableStream);
	});

	test("incoming binary (ArrayBuffer) and text surface on the readable", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.open();
		const { readable } = await wss.opened;
		const reader = readable.getReader();

		ws.message(new Uint8Array([1, 2, 3]).buffer);
		ws.message("hello");

		const a = await reader.read();
		const b = await reader.read();
		expect(a.value).toEqual(new Uint8Array([1, 2, 3]));
		expect(b.value).toBe("hello");
	});

	test("writes are sent and backpressure follows bufferedAmount", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor, highWaterMark: 100 });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.open();
		const { writable } = await wss.opened;
		const writer = writable.getWriter();

		// Under the mark: write resolves immediately.
		await writer.write(new Uint8Array([1]));
		expect(ws.sent.length).toBe(1);

		// Over the mark: the next write's drain keeps `ready` pending until it falls.
		ws.bufferedAmount = 1000;
		const pending = writer.write(new Uint8Array([2]));
		let settled = false;
		void pending.then(() => {
			settled = true;
		});
		await tick(5);
		expect(settled).toBe(false); // still backpressured
		expect(ws.sent.length).toBe(2); // but the bytes were already sent

		ws.bufferedAmount = 0; // drained
		await pending;
		expect(settled).toBe(true);
	});

	test("setHighWaterMark resizes backpressure live (mid-drain)", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor, highWaterMark: 100 });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.open();
		const { writable } = await wss.opened;
		const writer = writable.getWriter();

		ws.bufferedAmount = 500; // over the 100-byte mark
		const pending = writer.write(new Uint8Array([1]));
		let settled = false;
		void pending.then(() => {
			settled = true;
		});
		await tick(5);
		expect(settled).toBe(false); // backpressured at 100

		// Raise the mark above the buffered amount — the in-progress drain resolves
		// without bufferedAmount having to change.
		wss.setHighWaterMark(1000);
		expect(wss.highWaterMark).toBe(1000);
		await pending;
		expect(settled).toBe(true);
	});

	test("close resolves the closed promise and shuts the readable", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.open();
		const { readable } = await wss.opened;
		const reader = readable.getReader();

		ws.fireClose(1000, "bye");
		const info = await wss.closed;
		expect(info).toEqual({ closeCode: 1000, reason: "bye" });
		expect((await reader.read()).done).toBe(true);
	});

	test("error before open rejects opened", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.error();
		await expect(wss.opened).rejects.toThrow();
	});

	test("close() guards invalid WebSocket close codes", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws = FakeWebSocket.last as FakeWebSocket;

		wss.close({ closeCode: 0, reason: "nope" }); // 0 is invalid → bare close
		expect(ws.closeArgs).toEqual({ code: undefined, reason: undefined });

		ws.closeArgs = undefined;
		const wss2 = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws2 = FakeWebSocket.last as FakeWebSocket;
		wss2.close({ closeCode: 1000, reason: "ok" }); // valid → forwarded
		expect(ws2.closeArgs).toEqual({ code: 1000, reason: "ok" });
	});

	test("closing the writable closes the underlying socket", async () => {
		const wss = new WebSocketStream("wss://x", { webSocket: ctor });
		const ws = FakeWebSocket.last as FakeWebSocket;
		ws.open();
		const { writable } = await wss.opened;
		await writable.getWriter().close();
		expect(ws.closeArgs).toBeDefined();
	});

	test("openWebSocketStream uses the ponyfill when there's no native global", () => {
		const wss = openWebSocketStream("wss://x", { webSocket: ctor });
		expect(wss).toBeInstanceOf(WebSocketStream);
	});
});

describe("WebSocketStream.adopt", () => {
	/** A socket handed over by a server after the upgrade: already OPEN, with the
	 *  subprotocol chosen during the handshake. */
	function accepted(protocol = "p1"): FakeWebSocket {
		const ws = new FakeWebSocket("wss://x/y", protocol);
		ws.readyState = 1; // OPEN
		return ws;
	}

	test("an already-open socket resolves opened without an onopen event", async () => {
		// The server's socket opened before we ever saw it, so `onopen` has already
		// fired (or never will). Waiting for it would hang the session forever.
		const wss = WebSocketStream.adopt(accepted());
		const { protocol } = await wss.opened;
		expect(protocol).toBe("p1");
	});

	test("adopted sockets carry data both ways", async () => {
		const ws = accepted();
		const wss = WebSocketStream.adopt(ws);
		const { readable, writable } = await wss.opened;

		await writable.getWriter().write(new Uint8Array([1, 2, 3]));
		expect(ws.sent).toEqual([new Uint8Array([1, 2, 3])]);

		ws.message(new Uint8Array([4, 5]).buffer);
		const { value } = await readable.getReader().read();
		expect(value).toEqual(new Uint8Array([4, 5]));
	});

	test("url and binaryType come off the adopted socket", async () => {
		const ws = accepted();
		const wss = WebSocketStream.adopt(ws);
		expect(wss.url).toBe("wss://x/y");
		expect(ws.binaryType).toBe("arraybuffer");
	});

	test("a socket still connecting waits for onopen", async () => {
		// Deno hands back a CONNECTING socket: the upgrade response hasn't been
		// written yet. Settling `opened` from readyState here would be premature.
		const ws = new FakeWebSocket("wss://x/y", "p1");
		const wss = WebSocketStream.adopt(ws);
		let opened = false;
		void wss.opened.then(() => {
			opened = true;
		});
		await tick();
		expect(opened).toBe(false);

		ws.open();
		await wss.opened;
		expect(opened).toBe(true);
	});

	test("adopting a closed socket rejects opened rather than hanging", async () => {
		const ws = new FakeWebSocket("wss://x/y", "p1");
		ws.readyState = 3; // CLOSED
		const wss = WebSocketStream.adopt(ws);
		expect(wss.opened).rejects.toThrow(/already closed/);
		expect(await wss.closed).toEqual({ closeCode: 1006, reason: "" });
	});

	test("backpressure applies to an adopted socket", async () => {
		const ws = accepted();
		ws.bufferedAmount = 100;
		const wss = WebSocketStream.adopt(ws, { highWaterMark: 10 });
		expect(wss.highWaterMark).toBe(10);

		const { writable } = await wss.opened;
		let written = false;
		void writable
			.getWriter()
			.write(new Uint8Array([1]))
			.then(() => {
				written = true;
			});
		await tick(2);
		expect(written).toBe(false); // parked until the buffer drains

		// The ponyfill re-checks `bufferedAmount` on a timer, so poll rather than
		// assume how many turns the drain takes.
		ws.bufferedAmount = 0;
		const deadline = Instant.add(now(), Nano.fromMilli(Milli(1000)));
		while (!written && now() < deadline) await tick();
		expect(written).toBe(true);
	});
});
