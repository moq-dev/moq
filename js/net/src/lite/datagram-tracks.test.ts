import { expect, test } from "bun:test";
import { accept, connect } from "../connection/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { sendOrder } from "../priority.ts";
import { Timescale, Timestamp } from "../time.ts";
import { wireOf } from "../wire.ts";
import { ALPN_04, ALPN_05, ALPN_06, ALPN_07_WIP } from "./version.ts";

const url = new URL("https://localhost/datagrams");
const path = Path.from("live");

for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	for (const streams of [false, true]) {
		test(`${alpn}: ${streams ? "same-name kinds" : "datagram-only track"} delivery`, async () => {
			const source = new Origin();
			const publication = source.createBroadcast(path);
			const datagrams = publication.broadcast.createDatagrams("audio", {
				timescale: Timescale.MICRO,
				priority: -7,
			});
			const track = streams
				? publication.broadcast.createTrack("audio", { timescale: Timescale.MILLI })
				: undefined;
			publication.announce();
			const pair = createMockTransportPair(alpn);
			const peers = await Promise.all([
				connect({ url, transport: pair.client }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			const remote = wireOf(peers[0]).consume(path);
			const reader = remote.datagrams("audio");
			reader.setPriority(17);
			try {
				const info = await reader.info();
				while (datagrams.priority.peek() !== 17) await datagrams.priority.changed();
				expect(info.priority).toBe(streams && alpn !== ALPN_07_WIP ? 0 : -7);
				const stamp = Timestamp.fromMicros(1234567);
				const sequence = (1n << 53n) + 1n;
				datagrams.insert({ sequence, timestamp: stamp, payload: new Uint8Array([7, 8]) });
				const received = await reader.recv();
				expect(received?.sequence).toBe(sequence);
				expect(received?.timestamp?.asMicros()).toBe(streams && alpn !== ALPN_07_WIP ? 1234000 : 1234567);
				expect(received?.payload).toEqual(new Uint8Array([7, 8]));
				if (track) {
					const grouped = remote.track("audio").subscribe();
					track.writeFrame({ timestamp: Timestamp.fromMillis(1), payload: new Uint8Array([9]) });
					expect((await grouped.recvGroup())?.sequence).toBe(0n);
					grouped.close();
				}
				reader.setPriority(-9);
				while (datagrams.priority.peek() !== -9) await datagrams.priority.changed();
				datagrams.finish();
				track?.finish();
				expect(await reader.recv()).toBeUndefined();
			} finally {
				reader.close();
				remote.close();
				for (const peer of peers) peer.abort();
				source.close();
			}
		});
	}
}

for (const options of [{ datagrams: false }, { datagramReadable: false }]) {
	test(`unsupported datagram receive setup is refused (${JSON.stringify(options)})`, async () => {
		const pair = createMockTransportPair(ALPN_07_WIP, options);
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server }),
		]);
		const remote = wireOf(client).consume(path);
		const reader = remote.datagrams("missing");
		try {
			await expect(reader.info()).rejects.toThrow("does not support datagram");
		} finally {
			reader.close();
			remote.close();
			client.abort();
			server.abort();
		}
	});
}

test("lite-04 refuses datagram tracks at setup", async () => {
	const pair = createMockTransportPair(ALPN_04);
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server }),
	]);
	const remote = wireOf(client).consume(path);
	const reader = remote.datagrams("missing");
	try {
		await expect(reader.info()).rejects.toThrow("does not support datagram");
	} finally {
		reader.close();
		remote.close();
		client.abort();
		server.abort();
	}
});

test("lite-07 bounds and prioritizes independent createWritable datagram queues", async () => {
	const source = new Origin();
	const publication = source.createBroadcast(path);
	const datagrams = publication.broadcast.createDatagrams("audio");
	publication.broadcast.createTrack("audio", { compress: true });
	publication.announce();
	const other = publication.broadcast.createDatagrams("other", { priority: 7 });
	const pair = createMockTransportPair(ALPN_07_WIP, { datagramWritable: "createWritable" });
	Object.defineProperty(pair.server.datagrams, "maxDatagramSize", { value: 900 });
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(client).consume(path);
	const reader = remote.datagrams("audio");
	const second = remote.datagrams("other");
	reader.setPriority(19);
	second.setPriority(31);
	try {
		await reader.info();
		while (datagrams.priority.peek() !== 19) await datagrams.priority.changed();
		await second.info();
		other.append(undefined, new Uint8Array([1]));
		await second.recv();
		expect(pair.server.sendStreams.datagrams).toHaveLength(2);
		const queue = pair.server.sendStreams.datagrams.find(
			(queue) => queue.sendOrder === sendOrder({ priority: 19 }),
		);
		expect(queue).toBeDefined();
		reader.setPriority(-11);
		while (datagrams.priority.peek() !== -11) await datagrams.priority.changed();
		expect(queue?.sendOrder).toBe(sendOrder({ priority: -11 }));
		expect(pair.server.sendStreams.datagrams.find((other) => other !== queue)?.sendOrder).toBe(
			sendOrder({ priority: 31, publisher: 7 }),
		);
		// Type, Subscribe ID, and Sequence each occupy one byte on this untimed track.
		datagrams.append(undefined, new Uint8Array(898));
		datagrams.append(undefined, new Uint8Array(897));
		expect((await reader.recv())?.sequence).toBe(1n);
	} finally {
		second.close();
		reader.close();
		remote.close();
		client.abort();
		server.abort();
		source.close();
	}
});

for (const alpn of [ALPN_05, ALPN_06])
	for (const first of ["stream", "datagram"]) {
		test(`${alpn}: datagram readers rejoin and outlive streams (${first} first)`, async () => {
			const source = new Origin();
			const publication = source.createBroadcast(path);
			const datagrams = publication.broadcast.createDatagrams("audio");
			const track = publication.broadcast.createTrack("audio");
			publication.announce();
			const pair = createMockTransportPair(alpn);
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			const remote = wireOf(client).consume(path);
			let reader = first === "datagram" ? remote.datagrams("audio") : undefined;
			const grouped = remote.track("audio").subscribe();
			try {
				await grouped.info();
				track.writeFrame({ payload: new Uint8Array([1]) });
				(await grouped.recvGroup())?.close();
				reader ??= remote.datagrams("audio");
				await reader.info();
				reader.setPriority(37);
				while (datagrams.priority.peek() !== 37) await datagrams.priority.changed();
				reader.close();
				while (datagrams.priority.peek() !== 0) await datagrams.priority.changed();
				reader = remote.datagrams("audio");
				await reader.info();
				reader.setPriority(37);
				while (datagrams.priority.peek() !== 37) await datagrams.priority.changed();
				grouped.close();
				datagrams.append(undefined, new Uint8Array([2]));
				expect((await reader.recv())?.payload).toEqual(new Uint8Array([2]));
				datagrams.finish();
				track.finish();
				expect(await reader.recv()).toBeUndefined();
			} finally {
				reader?.close();
				grouped.close();
				remote.close();
				client.abort();
				server.abort();
				source.close();
			}
		});
	}

test("lite-07 forwards a priority change made while the previous DATAGRAMS_UPDATE writes", async () => {
	const source = new Origin();
	const publication = source.createBroadcast(path);
	const datagrams = publication.broadcast.createDatagrams("audio");
	publication.announce();
	const pair = createMockTransportPair(ALPN_07_WIP);
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(client).consume(path);
	const reader = remote.datagrams("audio");
	try {
		await reader.info();
		// Land the second change at each point of the first update's write.
		for (let yields = 0; yields < 40; yields++) {
			reader.setPriority(yields + 1);
			for (let i = 0; i < yields; i++) await Promise.resolve();
			reader.setPriority(-(yields + 1));
			// Without a recheck after the write, the publisher stays at the first value and this hangs.
			while (datagrams.priority.peek() !== -(yields + 1)) await datagrams.priority.changed();
		}
	} finally {
		reader.close();
		remote.close();
		client.abort();
		server.abort();
		source.close();
	}
});

test("lite-06: a request cancelled behind the shared datagram writer sends nothing", async () => {
	const source = new Origin();
	const publication = source.createBroadcast(path);
	const first = publication.broadcast.createDatagrams("a");
	const second = publication.broadcast.createDatagrams("b");
	publication.announce();
	const pair = createMockTransportPair(ALPN_06);
	// The legacy API's one session queue, stalled on its first write.
	const sent: Uint8Array[] = [];
	const stall = Promise.withResolvers<void>();
	Object.defineProperty(pair.server.datagrams, "writable", {
		value: new WritableStream<Uint8Array>({
			write(chunk) {
				sent.push(chunk);
				return sent.length === 1 ? stall.promise : undefined;
			},
		}),
	});
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(client).consume(path);
	const a = remote.datagrams("a");
	const b = remote.datagrams("b");
	const settle = async () => {
		for (let i = 0; i < 20; i++) await new Promise((resolve) => setImmediate(resolve));
	};
	try {
		await Promise.all([a.info(), b.info()]);
		while (first.priority.peek() === undefined || second.priority.peek() === undefined)
			await Promise.race([first.priority.changed(), second.priority.changed()]);
		first.append(undefined, Uint8Array.of(0xaa));
		while (sent.length === 0) await settle();
		// The second request now waits on the writer the first one holds.
		second.append(undefined, Uint8Array.of(0xbb));
		await settle();
		b.close();
		while (second.priority.peek() !== undefined) await second.priority.changed();
		stall.resolve();
		await settle();
		expect(sent.map((chunk) => chunk.at(-1))).toEqual([0xaa]);
	} finally {
		stall.resolve();
		a.close();
		b.close();
		remote.close();
		client.abort();
		server.abort();
		source.close();
	}
});
