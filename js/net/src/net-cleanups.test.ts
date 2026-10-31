import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { accept } from "./connection/accept.ts";
import { connect } from "./connection/connect.ts";
import { ConnectTimeout } from "./error.ts";
import { Producer as GroupProducer } from "./group.ts";
import { Connection as LiteConnection } from "./lite/connection.ts";
import { ALPN_03, ALPN_04, Version } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as TrackProducer } from "./track.ts";

test("group read handles independently replay buffered and future frames", async () => {
	const group = new GroupProducer(0);
	const first = group.consume();
	const payload = (text: string) => ({ payload: new TextEncoder().encode(text) });
	try {
		group.writeFrame(payload("a"));
		expect(await first.readString()).toBe("a");
		const second = group.consume();
		try {
			expect(second.tryReadFrame()?.payload).toEqual(payload("a").payload);
			group.writeFrame(payload("b"));
			expect(await second.readString()).toBe("b");
			expect(await first.readString()).toBe("b");
			first.close();
			group.writeFrame(payload("c"));
			expect(await second.readString()).toBe("c");
			group.finish();
			expect(await second.readFrame()).toBeUndefined();
		} finally {
			second.close();
		}
	} finally {
		first.close();
		group.finish();
	}
});

test.each(["group", "track"] as const)("%s JSON rejects an empty frame without treating it as EOF", async (kind) => {
	const producer = new TrackProducer("json");
	const subscriber = kind === "track" ? producer.subscribe().ordered() : undefined;
	const group = kind === "track" ? producer.createGroup(0) : new GroupProducer(0);
	const reader = subscriber ?? group.consume();
	group.writeFrame({ payload: new Uint8Array() });
	for (const value of [null, false, 0, ""])
		group.writeFrame({ payload: new TextEncoder().encode(JSON.stringify(value)) });
	group.finish();
	producer.finish();
	producer.close();
	try {
		await expect(reader.readJson()).rejects.toThrow(SyntaxError);
		for (const value of [null, false, 0, ""]) expect(await reader.readJson()).toBe(value);
		expect(await reader.readJson()).toBeUndefined();
	} finally {
		reader.close();
	}
});

test("group booleans refuse every shape except one byte of zero or one", async () => {
	const group = new GroupProducer(0);
	const reader = group.consume();
	for (const bytes of [[], [2], [1, 0], [0], [1]]) group.writeFrame({ payload: new Uint8Array(bytes) });
	group.finish();
	try {
		for (let i = 0; i < 3; i++) await expect(reader.readBool()).rejects.toThrow("invalid bool frame");
		expect(await reader.readBool()).toBe(false);
		expect(await reader.readBool()).toBe(true);
		expect(await reader.readBool()).toBeUndefined();
	} finally {
		reader.close();
	}
});

for (const [version, alpn] of [
	[Version.DRAFT_03, ALPN_03],
	[Version.DRAFT_04, ALPN_04],
] as const) {
	test(`${alpn} two-Connection peers receive PROBE without waiting for nonexistent SETUP`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const pair = createMockTransportPair(alpn, { stats: { estimatedSendRate: 8_000_000, smoothedRtt: 12 } });
		const props = { url: new URL("https://example.com"), version };
		const client = new LiteConnection({ ...props, quic: pair.client });
		const server = new LiteConnection({ ...props, quic: pair.server });
		const dispose = client.probe.subscribe(() => {});
		try {
			// The publisher samples on its 100ms PROBE cadence.
			await clock.advance(Nano.fromMilli(Milli(100)));
			expect(client.probe.peek().estimatedRecvRate).toBe(8_000_000);
			expect(client.probe.peek().rtt).toBe(version === Version.DRAFT_03 ? undefined : Milli(12));
		} finally {
			dispose();
			client.abort();
			server.abort();
			installed.close();
		}
	});
}

test("accept applies its default handshake deadline and releases a silent transport", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair("moqt-18");
	let outcome: unknown;
	const pending = accept({ transport: pair.server, url: new URL("https://example.com") }).then(
		(value) => {
			outcome = value;
		},
		(error) => {
			outcome = error;
		},
	);
	try {
		await clock.advance(Nano.fromMilli(Milli(29_999)));
		expect(outcome).toBeUndefined();
		await clock.advance(Nano.fromMilli(Milli(1)));
		expect(outcome).toBeInstanceOf(ConnectTimeout);
		expect((await pair.client.closed).closeCode).toBe(0);
	} finally {
		pair.client.close();
		pair.server.close();
		await pending;
		installed.close();
	}
});

test("accept releases the transport when protocol selection fails", async () => {
	const pair = createMockTransportPair("unsupported");
	const closing = spyOn(pair.server, "close");
	try {
		await expect(accept({ transport: pair.server, url: new URL("https://example.com") })).rejects.toThrow(
			"unsupported WebTransport protocol",
		);
		expect(closing).toHaveBeenCalledTimes(1);
	} finally {
		pair.client.close();
		pair.server.close();
		closing.mockRestore();
	}
});

test("malformed SETUP closes the accepted transport", async () => {
	const pair = createMockTransportPair("");
	const pending = accept({ transport: pair.server, url: new URL("https://example.com") }).catch(
		(error: unknown) => error,
	);
	try {
		const stream = await pair.client.createBidirectionalStream();
		const writer = stream.writable.getWriter();
		try {
			await writer.write(new Uint8Array([0]));
			expect(await pending).toEqual(new Error("unexpected SETUP message type: 0x0"));
			expect((await pair.client.closed).closeCode).toBe(0);
		} finally {
			writer.releaseLock();
		}
	} finally {
		pair.client.close();
		pair.server.close();
	}
});

test("zero accept timeout waits for caller cancellation and preserves its reason", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair("moqt-18");
	const controller = new AbortController();
	const reason = new Error("caller cancelled SETUP");
	let outcome: unknown;
	const pending = accept({
		transport: pair.server,
		url: new URL("https://example.com"),
		timeout: Milli(0),
		signal: controller.signal,
	}).then(
		(value) => {
			outcome = value;
		},
		(error) => {
			outcome = error;
		},
	);
	try {
		await clock.advance(Nano.fromMilli(Milli(60_000)));
		expect(outcome).toBeUndefined();
		controller.abort(reason);
		await pending;
		expect(outcome).toBe(reason);
		expect((await pair.client.closed).closeCode).toBe(0);
	} finally {
		pair.client.close();
		pair.server.close();
		await pending;
		installed.close();
	}
});

test("invalid or pre-aborted accept options never take ownership of the transport", async () => {
	const pair = createMockTransportPair("moqt-18");
	const closing = spyOn(pair.server, "close");
	try {
		for (const timeout of [-1, Number.NaN, Number.POSITIVE_INFINITY]) {
			await expect(
				accept({ transport: pair.server, url: new URL("https://example.com"), timeout: timeout as Milli }),
			).rejects.toThrow("invalid time");
		}
		const reason = new Error("already cancelled");
		await expect(
			accept({ transport: pair.server, url: new URL("https://example.com"), signal: AbortSignal.abort(reason) }),
		).rejects.toBe(reason);
		expect(closing).not.toHaveBeenCalled();
	} finally {
		closing.mockRestore();
		pair.client.close();
		pair.server.close();
	}
});

test.each([ALPN_04, "moqt-18"])("accepted %s sessions outlive their handshake timeout and signal", async (alpn) => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(alpn);
	const controller = new AbortController();
	const closing = spyOn(pair.server, "close");
	const url = new URL("https://example.com");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, timeout: Milli(1_000), signal: controller.signal }),
	]);
	try {
		controller.abort();
		await clock.advance(Nano.fromMilli(Milli(5_000)));
		expect(closing).not.toHaveBeenCalled();
		expect(await server.stats()).toEqual({});
	} finally {
		closing.mockRestore();
		client.abort();
		server.abort();
		installed.close();
	}
});
