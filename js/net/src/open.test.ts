import { expect, test } from "bun:test";
import Qmux from "@moq/qmux";
import { Version } from "./lite/version.ts";
import { createMockTransportPair } from "./mock.ts";
import { lines } from "./open.ts";
import { Writer } from "./stream.ts";

const version = Version.DRAFT_07;
const never = new Promise<void>(() => {});

// A qmux session pair over an in-memory socket.
function qmuxPair(): { client: WebTransport; server: WebTransport } {
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
	const server = Qmux.accept(socket(up.readable, down.writable), { config });
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
