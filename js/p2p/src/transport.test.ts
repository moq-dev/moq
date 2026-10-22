import { expect, test } from "bun:test";
import { Channel } from "./channel.ts";
import { peers } from "./mock.ts";
import { Transport } from "./transport.ts";

const protocol = "moq-lite-07-wip";

function pair(maxMessageSize = 65536) {
	const mock = peers(maxMessageSize);
	const left = Transport.connect({ peer: mock.left.asPeer(), protocol });
	const right = Transport.accept({ peer: mock.right.asPeer(), channel: mock.right.incoming.asChannel(), protocol });
	mock.left.channel.open();
	mock.right.incoming.open();
	return { ...mock, client: left, server: right };
}

async function next<T>(stream: ReadableStream<T>): Promise<T> {
	const reader = stream.getReader();
	try {
		const { value, done } = await reader.read();
		if (done) throw new Error("stream ended before its item");
		return value;
	} finally {
		reader.releaseLock();
	}
}

async function readAll(stream: ReadableStream<Uint8Array>): Promise<Uint8Array> {
	const reader = stream.getReader();
	const chunks: Uint8Array[] = [];
	let size = 0;
	while (true) {
		const { value, done } = await reader.read();
		if (done) break;
		chunks.push(value);
		size += value.byteLength;
	}
	const result = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

test("qmux carries large streams in bounded binary records and preserves both stream-id roles", async () => {
	for (const max of [16382, 16384, 65536, Infinity]) {
		const { client, server, left, right } = pair(max);
		await Promise.all([client.ready, server.ready]);
		expect(left.options).toEqual({ ordered: true, protocol });
		expect(left.channel.binaryType).toBe("arraybuffer");
		expect(client.datagrams.maxDatagramSize).toBe(0);
		const payload = Uint8Array.from({ length: 256 * 1024 }, (_, i) => i % 251);
		const receiving = next(server.incomingUnidirectionalStreams).then(readAll);
		const writer = (await client.createUnidirectionalStream()).getWriter();
		await writer.write(payload);
		await writer.close();
		expect(await receiving).toEqual(payload);
		const bidiIncoming = next(client.incomingBidirectionalStreams);
		const bidi = await server.createBidirectionalStream();
		const response = bidi.writable.getWriter();
		await response.write(new Uint8Array([4, 2]));
		await response.close();
		expect(await readAll((await bidiIncoming).readable)).toEqual(new Uint8Array([4, 2]));
		expect(left.channel.records.every((record) => record.byteLength <= Math.min(16384, max))).toBe(true);
		client.close({ closeCode: 42, reason: "bye" });
		expect(await server.closed).toEqual({ closeCode: 42, reason: "bye" });
		await client.closed;
		expect(left.closed).toBe(false);
		expect(right.closed).toBe(false);
		expect(left.channel.listeners.size).toBe(0);
		expect(right.incoming.listeners.size).toBe(0);
	}
});

test("close before the channel opens settles closed and rejects ready without closing the peer", async () => {
	const { left } = peers();
	const transport = Transport.connect({ peer: left.asPeer(), protocol });
	transport.close({ closeCode: 7, reason: "cancelled" });
	expect(await transport.closed).toEqual({ closeCode: 7, reason: "cancelled" });
	await expect(transport.ready).rejects.toThrow("closed");
	expect(left.channel.readyState).toBe("closed");
	expect(left.closed).toBe(false);
	expect(left.channel.listeners.size).toBe(0);
});

test("accept refuses partial reliability, unordered delivery, and a different negotiated protocol", () => {
	for (const change of [
		{ ordered: false },
		{ maxRetransmits: 1 },
		{ maxPacketLifeTime: 10 },
		{ protocol: "other" },
	]) {
		const { left, right } = peers();
		left.createDataChannel("moq", { ordered: true, protocol });
		Object.assign(right.incoming, change);
		expect(() =>
			Transport.accept({ peer: right.asPeer(), channel: right.incoming.asChannel(), protocol }),
		).toThrow();
	}
	expect(() => Transport.connect({ peer: peers().left.asPeer(), protocol: "" })).toThrow("ALPN");
});

test("an SCTP message limit below qmux's protocol minimum is refused", async () => {
	const { client, server, left } = pair(8192);
	await expect(client.ready).rejects.toThrow("minimum");
	await expect(server.ready).rejects.toThrow();
	await expect(client.closed).rejects.toThrow("minimum");
	expect(left.channel.listeners.size).toBe(0);
});

test("writer.ready follows bufferedamountlow and closing releases a blocked write", async () => {
	const { left } = peers();
	const channel = left.createDataChannel("moq", { ordered: true, protocol });
	left.channel.autoDrain = false;
	const socket = new Channel(left.asPeer(), channel, protocol);
	left.channel.open();
	const { writable } = await socket.opened;
	const writer = writable.getWriter();
	const record = new Uint8Array(16384);
	await writer.write(record);
	await writer.write(record);
	const pending = writer.write(record);
	let sent = false;
	void pending.then(
		() => {
			sent = true;
		},
		() => {},
	);
	await Promise.resolve();
	await Promise.resolve();
	expect(writer.desiredSize).toBe(0);
	expect(sent).toBe(false);
	left.channel.drain();
	await pending;
	expect(sent).toBe(true);
	await writer.write(record);
	await writer.write(record);
	const blocked = writer.write(record);
	await Promise.resolve();
	socket.close();
	await expect(blocked).rejects.toThrow("closed");
	expect(left.channel.listeners.size).toBe(0);
});

test("malformed binary input and an overflowing receive queue close the channel", async () => {
	for (const data of ["text", new ArrayBuffer(16385), new ArrayBuffer(16384)]) {
		const { left } = peers();
		const channel = left.createDataChannel("moq", { ordered: true, protocol });
		const socket = new Channel(left.asPeer(), channel, protocol);
		left.channel.open();
		await socket.opened;
		if (data instanceof ArrayBuffer && data.byteLength === 16384) {
			for (let i = 0; i < 4; i++) left.channel.dispatchEvent(new MessageEvent("message", { data }));
		}
		left.channel.dispatchEvent(new MessageEvent("message", { data }));
		await expect(socket.closed).rejects.toThrow();
		expect(left.channel.listeners.size).toBe(0);
	}
});

test("incoming stream cancellation waits for the channel to open", async () => {
	const mock = peers();
	const client = Transport.connect({ peer: mock.left.asPeer(), protocol });
	const server = Transport.accept({ peer: mock.right.asPeer(), channel: mock.right.incoming.asChannel(), protocol });
	const cancelled = client.incomingUnidirectionalStreams.cancel();
	mock.left.channel.open();
	mock.right.incoming.open();
	await cancelled;
	await Promise.all([client.ready, server.ready]);
	client.close();
	await Promise.all([client.closed, server.closed]);
});
