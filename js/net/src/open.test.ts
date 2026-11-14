import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Session as Qmux } from "@moq/qmux";
import { Version } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { lines } from "./open.ts";
import { Writer } from "./stream.ts";

const version = Version.DRAFT_07;
const never = new Promise<void>(() => {});

// A qmux session pair over an in-memory socket, the server granting `maxStreamsUni`.
function qmuxPair(maxStreamsUni?: bigint): { client: WebTransport; server: WebTransport } {
	const socket = (readable: ReadableStream<Uint8Array>, writable: WritableStream<Uint8Array>) => {
		const closed = Promise.withResolvers<{ closeCode: number; reason: string }>();
		return {
			url: "",
			opened: Promise.resolve({ readable, writable, extensions: "", protocol: "qmux-01" }),
			closed: closed.promise,
			close: () => closed.resolve({ closeCode: 1000, reason: "" }),
		};
	};
	// Copied, since a sender may reuse its buffer once a write resolves.
	const pipe = () =>
		new TransformStream<Uint8Array, Uint8Array>({ transform: (chunk, c) => c.enqueue(chunk.slice()) });
	const up = pipe();
	const down = pipe();
	const config = { idleTimeout: null };
	const client = new Qmux(socket(down.readable, up.writable), { config });
	const server = Qmux.accept(socket(up.readable, down.writable), { config: { ...config, maxStreamsUni } });
	return { client, server };
}

function openGroup(quic: WebTransport, sendOrder: number): Promise<Writer> {
	return Writer.tryOpen(quic, { version, sendOrder, cancel: never, group: true }) as Promise<Writer>;
}

function orders(quic: WebTransport): number[] {
	return lines(quic)
		.streams.list()
		.map((stream) => stream.sendOrder);
}

test("group streams are listed lowest send order first until they close or reset", async () => {
	const { client, server } = createMockTransportPair();
	const first = await openGroup(client, 1);
	const second = await openGroup(client, 3);
	const control = await Writer.open(client, { version });
	const third = await openGroup(client, 2);
	expect(orders(client)).toEqual([1, 2, 3]);

	second.setPriority(0);
	expect(orders(client)).toEqual([0, 1, 2]);

	first.close();
	second.reset(new Error("cancel"));
	expect(orders(client)).toEqual([2]);
	third.close();
	expect(orders(client)).toEqual([]);

	expect(lines(client).streams.acknowledges).toBe(false);
	expect(await control.acknowledged()).toBeUndefined();
	client.close();
	server.close();
});

test("a qmux group stream reports what the peer has acknowledged", async () => {
	const { client, server } = qmuxPair();
	expect(lines(client).streams.acknowledges).toBe(true);
	const stream = await openGroup(client, 0);
	const [listed] = lines(client).streams.list();
	expect(listed).toBe(stream);
	expect(await listed.acknowledged()).toBe(true);

	const write = stream.write(new Uint8Array(1024));
	expect(await listed.acknowledged()).toBe(false);
	await write;
	expect(await listed.acknowledged()).toBe(true);

	stream.reset(new Error("cancel"));
	expect(await stream.acknowledged()).toBe(false);
	client.close();
	server.close();
});

test("an open waiting a round trip resets the lowest stream below it, one per waiting open", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { client, server } = qmuxPair(2n);
	try {
		const low = await openGroup(client, 1);
		const mid = await openGroup(client, 2);
		await low.write(new Uint8Array(16));
		await mid.write(new Uint8Array(16));
		// Before any RTT sample, an open waits 100 ms.
		const wait = () => clock.advance(Duration.millis(100));

		const below = Promise.withResolvers<void>();
		const pending = Writer.tryOpen(client, { version, sendOrder: 0, cancel: below.promise, group: true });
		await wait();
		expect(orders(client)).toEqual([1, 2]);
		below.resolve();
		expect(await pending).toBeUndefined();

		const high = openGroup(client, 5);
		await wait();
		expect(orders(client)).toEqual([2]);
		await expect(low.closed).rejects.toThrow("lagged");
		await wait();
		expect(orders(client)).toEqual([2]);

		const next = openGroup(client, 4);
		await wait();
		expect(orders(client)).toEqual([]);

		// The peer is done with both reset streams, returning their credit.
		const incoming = server.incomingUnidirectionalStreams.getReader();
		for (let i = 0; i < 2; i++) {
			const { value } = await incoming.read();
			await value?.cancel();
		}
		expect((await high).sendOrder).toBe(5);
		expect((await next).sendOrder).toBe(4);
	} finally {
		client.close();
		server.close();
		installed.close();
	}
});

test("an open makes room only once it has waited a round trip itself", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { client, server } = qmuxPair(1n);
	try {
		const low = await openGroup(client, 1);
		await low.write(new Uint8Array(16));
		const wait = (ms: number) => clock.advance(Duration.millis(ms));

		const below = Promise.withResolvers<void>();
		const pending = Writer.tryOpen(client, { version, sendOrder: 0, cancel: below.promise, group: true });
		await wait(50);
		const high = openGroup(client, 5);
		await wait(50);
		expect(orders(client)).toEqual([1]);
		await wait(50);
		expect(orders(client)).toEqual([]);

		below.resolve();
		expect(await pending).toBeUndefined();
		const incoming = server.incomingUnidirectionalStreams.getReader();
		const { value } = await incoming.read();
		await value?.cancel();
		expect((await high).sendOrder).toBe(5);
	} finally {
		client.close();
		server.close();
		installed.close();
	}
});

test("an open ranked above an impatient one makes room for it at once", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { client, server } = qmuxPair(2n);
	try {
		const low = await openGroup(client, 1);
		const mid = await openGroup(client, 2);
		await low.write(new Uint8Array(16));
		await mid.write(new Uint8Array(16));

		const waiting = openGroup(client, 5);
		await clock.advance(Duration.millis(100));
		expect(orders(client)).toEqual([2]);

		// The reset's credit will reach this open instead, so the one still waiting is owed another.
		const higher = openGroup(client, 10);
		// Well short of its own round trip.
		await clock.advance(Duration.millis(1));
		expect(orders(client)).toEqual([]);

		const incoming = server.incomingUnidirectionalStreams.getReader();
		for (let i = 0; i < 2; i++) {
			const { value } = await incoming.read();
			await value?.cancel();
		}
		expect((await higher).sendOrder).toBe(10);
		expect((await waiting).sendOrder).toBe(5);
	} finally {
		client.close();
		server.close();
		installed.close();
	}
});

test("an RTT of 0 reads as no sample, so an open still waits 100 ms", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const { client, server } = qmuxPair(1n);
	// A transport may report a zero smoothed RTT before its first sample.
	Object.assign(client, { getStats: async () => ({ smoothedRtt: 0 }) });
	try {
		const low = await openGroup(client, 1);
		await low.write(new Uint8Array(16));

		const high = openGroup(client, 5);
		await clock.advance(Duration.millis(99));
		expect(orders(client)).toEqual([1]);
		await clock.advance(Duration.millis(1));
		await expect(low.closed).rejects.toThrow("lagged");

		const incoming = server.incomingUnidirectionalStreams.getReader();
		const { value } = await incoming.read();
		await value?.cancel();
		expect((await high).sendOrder).toBe(5);
	} finally {
		client.close();
		server.close();
		installed.close();
	}
});
