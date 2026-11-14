import { expect, onTestFinished, spyOn, test } from "bun:test";
import * as clock from "@moq/clock";
import { yieldNow } from "@moq/clock";
import { Signal } from "@moq/signals";
import { accept, connect } from "../connection/index.ts";
import { SessionCode } from "../error.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair, type MockTransport, manual } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { sendOrder } from "../priority.ts";
import { Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { Budget } from "../util/yield.ts";
import { wireOf } from "../wire.ts";
import { DataType } from "./stream.ts";
import { ALPN_04, ALPN_05, ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost/datagrams");
const path = Path.from("live");

for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	for (const streams of [false, true]) {
		test(`${alpn}: ${streams ? "same-name kinds" : "datagram-only track"} delivery`, async () => {
			using clock = manual();
			const source = new Origin();
			const publication = source.route(path, { hidden: true });
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
				// The end waits out the grace a late datagram gets.
				expect(await clock.drive(reader.recv())).toBeUndefined();
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
	const publication = source.route(path, { hidden: true });
	const datagrams = publication.broadcast.createDatagrams("audio");
	publication.broadcast.createTrack("audio", { compression: "deflate" });
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
		// The publisher opens a track's queue after writing its DATAGRAMS_INFO, so the reader can
		// see the info first: a datagram through each queue is what proves both are open.
		datagrams.append(Timestamp.unscaled(0n), new Uint8Array([0]));
		expect((await reader.recv())?.sequence).toBe(0n);
		await second.info();
		other.append(Timestamp.unscaled(0n), new Uint8Array([1]));
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
		datagrams.append(Timestamp.unscaled(1n), new Uint8Array(898));
		datagrams.append(Timestamp.unscaled(2n), new Uint8Array(897));
		expect((await reader.recv())?.sequence).toBe(2n);
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
			const publication = source.route(path, { hidden: true });
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
				track.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.unscaled(0n) });
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
				datagrams.append(Timestamp.unscaled(0n), new Uint8Array([2]));
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
	const publication = source.route(path, { hidden: true });
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
	const publication = source.route(path, { hidden: true });
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
		first.append(Timestamp.unscaled(0n), Uint8Array.of(0xaa));
		while (sent.length === 0) await settle();
		// The second request now waits on the writer the first one holds.
		second.append(Timestamp.unscaled(0n), Uint8Array.of(0xbb));
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

// A publisher's stream writes, handed to the subscriber `lag` microtasks late: the answer it
// writes lands after a datagram sent behind it, though in the same host task.
function lagAnswers(transport: MockTransport, lag: number): void {
	if (lag === 0) return;
	const lagging = (writable: WritableStream<Uint8Array>) => {
		const writer = writable.getWriter();
		let chain = Promise.resolve();
		const later = (op: () => Promise<void>) => {
			// A write the session's close fails is lost, as it would be on the wire.
			chain = chain
				.then(async () => {
					for (let i = 0; i < lag; i++) await Promise.resolve();
					await op();
				})
				.catch(() => {});
			return chain;
		};
		return new WritableStream<Uint8Array>({
			// Copied: the write resolves now, after which the stream may reuse its buffer.
			write: (chunk) => {
				const copy = chunk.slice();
				void later(() => writer.write(copy));
			},
			close: () => later(() => writer.close()),
			abort: (reason) => writer.abort(reason),
		});
	};
	const incoming = transport.incomingBidirectionalStreams.pipeThrough(
		new TransformStream<WebTransportBidirectionalStream, WebTransportBidirectionalStream>({
			transform: (bidi, controller) =>
				controller.enqueue({
					readable: bidi.readable,
					writable: lagging(bidi.writable),
				} as WebTransportBidirectionalStream),
		}),
	);
	Object.defineProperty(transport, "incomingBidirectionalStreams", { value: incoming });
}

// A publisher that writes as soon as a subscription reaches it sends the datagram right behind
// its answer, which may reach the subscriber first, with the track's end too when it finishes at
// once. The subscriber decodes the answer before it routes the datagram, and routes the datagram
// before it ends the track.
for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	for (const [lag, finish] of [
		[0, false],
		[100, false],
		[0, true],
		[100, true],
	] as const) {
		test(`${alpn}: a datagram sent right behind its answer arrives (answer ${lag} microtasks late${finish ? ", then the end" : ""})`, async () => {
			using clock = manual();
			const source = new Origin();
			const publication = source.route(path, { hidden: true });
			const datagrams = publication.broadcast.createDatagrams("audio");
			publication.announce();
			const pair = createMockTransportPair(alpn);
			lagAnswers(pair.server, lag);
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			const remote = wireOf(client).consume(path);
			const reader = remote.datagrams("audio");
			// Only the subscription carries this priority: a lite-06 TRACK lookup asks for none.
			reader.setPriority(5);
			try {
				while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
				datagrams.append(Timestamp.unscaled(0n), Uint8Array.of(1));
				if (finish) datagrams.finish();
				await reader.info();
				if (!finish) datagrams.append(Timestamp.unscaled(1n), Uint8Array.of(2));
				expect((await reader.recv())?.payload).toEqual(Uint8Array.of(1));
				if (finish) {
					// The end waits out the grace a late datagram gets.
					expect(await clock.drive(reader.recv())).toBeUndefined();
				} else {
					expect((await reader.recv())?.payload).toEqual(Uint8Array.of(2));
				}
				expect((await client.stats()).datagramsUnrouted).toBe(0);
			} finally {
				reader.close();
				remote.close();
				client.abort();
				server.abort();
				source.close();
			}
		});
	}
}

// A publisher that writes a last datagram and finishes at once sends the datagram right ahead
// of its track's end. The subscriber routes the datagram before it ends the track.
for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	test(`${alpn}: a datagram sent right before its end arrives`, async () => {
		using clock = manual();
		const source = new Origin();
		const publication = source.route(path, { hidden: true });
		const datagrams = publication.broadcast.createDatagrams("audio");
		publication.announce();
		const pair = createMockTransportPair(alpn);
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: source.consume() }),
		]);
		const remote = wireOf(client).consume(path);
		const reader = remote.datagrams("audio");
		reader.setPriority(5);
		try {
			await reader.info();
			while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
			datagrams.append(Timestamp.unscaled(0n), Uint8Array.of(1));
			datagrams.finish();
			expect((await reader.recv())?.payload).toEqual(Uint8Array.of(1));
			// The end waits out the grace a late datagram gets.
			expect(await clock.drive(reader.recv())).toBeUndefined();
			expect((await client.stats()).datagramsUnrouted).toBe(0);
		} finally {
			reader.close();
			remote.close();
			client.abort();
			server.abort();
			source.close();
		}
	});
}

// A burst written right before a track's end can outlast the receive loop's budget, which yields
// with some of it still queued. The end waits for the loop to read the queue empty.
for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	test(`${alpn}: a burst sent right before its end arrives past a budget yield`, async () => {
		using clock = manual();
		// As many as a datagram track buffers unread, past two receive budgets.
		const BURST = 64;
		const source = new Origin();
		const publication = source.route(path, { hidden: true });
		const datagrams = publication.broadcast.createDatagrams("audio");
		publication.announce();
		const pair = createMockTransportPair(alpn);
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: source.consume() }),
		]);
		const remote = wireOf(client).consume(path);
		const reader = remote.datagrams("audio");
		reader.setPriority(5);
		// Every budget slice reads as spent, so the receive loop yields a host task every 32 checks.
		let checks = 0;
		const budget = spyOn(Budget.prototype, "poll").mockImplementation(() =>
			++checks % 32 === 0 ? yieldNow() : undefined,
		);
		try {
			await reader.info();
			while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
			for (let i = 0; i < BURST; i++) datagrams.append(Timestamp.unscaled(i), Uint8Array.of(i));
			datagrams.finish();
			const counted = (async () => {
				let count = 0;
				while ((await reader.recv()) !== undefined) count++;
				return count;
			})();
			// The end waits out the grace a late datagram gets.
			expect(await clock.drive(counted)).toBe(BURST);
			expect((await client.stats()).datagramsUnrouted).toBe(0);
		} finally {
			budget.mockRestore();
			reader.close();
			remote.close();
			client.abort();
			server.abort();
			source.close();
		}
	});
}

// A track's end waits a host task for the datagrams received with it. A session close in
// between finishes the track rather than cutting it off: the publisher already ended it.
for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	test(`${alpn}: a session close right after a datagram track's end keeps the end`, async () => {
		const source = new Origin();
		const publication = source.route(path, { hidden: true });
		const datagrams = publication.broadcast.createDatagrams("audio");
		publication.announce();
		const pair = createMockTransportPair(alpn);
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: source.consume() }),
		]);
		const remote = wireOf(client).consume(path);
		const reader = remote.datagrams("audio");
		reader.setPriority(5);
		// Only the end's routing yields a host task after the finish below: the receive loop's
		// budget never runs out here.
		const budget = spyOn(Budget.prototype, "poll").mockImplementation(() => undefined);
		let routing: ReturnType<typeof spyOn> | undefined;
		// Runs even when the test times out with a wait still pending.
		onTestFinished(() => {
			budget.mockRestore();
			routing?.mockRestore();
			reader.close();
			remote.close();
			client.abort();
			server.abort();
			source.close();
		});
		await reader.info();
		while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
		// The client marks the end taken, then waits a host task to route what arrived with it.
		const received = Promise.withResolvers<void>();
		const hostTask = clock.yieldNow;
		routing = spyOn(clock, "yieldNow").mockImplementation(() => {
			received.resolve();
			return hostTask();
		});
		datagrams.finish();
		await received.promise;
		client.abort();
		expect(await reader.recv()).toBeUndefined();
	});
}

// A peer's protocol violation fails the session at once, without the drain a close gets. A lite-05/06
// datagram track whose SUBSCRIBE_FINISH was accepted keeps that end, even while its FIN lags.
for (const [alpn, version] of [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
] as const) {
	test(`${alpn}: a duplicate SETUP after a datagram track's end keeps the end`, async () => {
		const source = new Origin();
		const publication = source.route(path, { hidden: true });
		const datagrams = publication.broadcast.createDatagrams("audio");
		publication.announce();
		const pair = createMockTransportPair(alpn);
		// Once armed, the publisher's FINs on the request streams it accepts never leave.
		let holding = false;
		const holdFin = (writable: WritableStream<Uint8Array>) => {
			const writer = writable.getWriter();
			return new WritableStream<Uint8Array>({
				write: (bytes) => writer.write(bytes),
				abort: (reason) => writer.abort(reason),
				close: () => (holding ? new Promise<void>(() => {}) : writer.close()),
			});
		};
		Object.defineProperty(pair.server, "incomingBidirectionalStreams", {
			value: pair.server.incomingBidirectionalStreams.pipeThrough(
				new TransformStream<WebTransportBidirectionalStream, WebTransportBidirectionalStream>({
					transform: (bidi, controller) =>
						controller.enqueue({ readable: bidi.readable, writable: holdFin(bidi.writable) }),
				}),
			),
		});
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: source.consume() }),
		]);
		const remote = wireOf(client).consume(path);
		const reader = remote.datagrams("audio");
		reader.setPriority(5);
		const accepted = Promise.withResolvers<void>();
		const endTrack = hooks.endTrack;
		const spy = spyOn(hooks, "endTrack").mockImplementation((...args) => {
			endTrack(...args);
			accepted.resolve();
		});
		try {
			await reader.info();
			while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
			holding = true;
			datagrams.finish();
			await accepted.promise;
			const writer = await Writer.open(pair.server, { version });
			// The client claims the Setup Stream on its type, so the type alone is the duplicate.
			await writer.u53(DataType.Setup);
			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			expect(await reader.recv()).toBeUndefined();
		} finally {
			spy.mockRestore();
			reader.close();
			remote.close();
			client.abort();
			server.abort();
			source.close();
		}
	});
}

// Holds back what the publisher writes on the next request stream it accepts once armed, until
// released: that subscription's answer stays outstanding while everything else flows.
function holdNextAnswer(transport: MockTransport): { arm(): void; release(): void } {
	let armed = false;
	const release = Promise.withResolvers<void>();
	const holding = (writable: WritableStream<Uint8Array>) => {
		const writer = writable.getWriter();
		let chain = release.promise;
		// A write the session's close fails is lost, as it would be on the wire.
		const later = (op: () => Promise<void>) => {
			chain = chain.then(op).catch(() => {});
			return chain;
		};
		return new WritableStream<Uint8Array>({
			// Copied: the write resolves now, after which the stream may reuse its buffer.
			write: (chunk) => {
				const copy = chunk.slice();
				void later(() => writer.write(copy));
			},
			close: () => void later(() => writer.close()),
			abort: (reason) => writer.abort(reason),
		});
	};
	const incoming = transport.incomingBidirectionalStreams.pipeThrough(
		new TransformStream<WebTransportBidirectionalStream, WebTransportBidirectionalStream>({
			transform: (bidi, controller) => {
				const held = armed;
				armed = false;
				controller.enqueue(
					held
						? ({
								readable: bidi.readable,
								writable: holding(bidi.writable),
							} as WebTransportBidirectionalStream)
						: bidi,
				);
			},
		}),
	);
	Object.defineProperty(transport, "incomingBidirectionalStreams", { value: incoming });
	return {
		arm: () => {
			armed = true;
		},
		release: () => release.resolve(),
	};
}

// Datagrams held for a track still awaiting its answer, more than are held at once, don't hold
// up an answered track's last datagram, which its end, arriving with it, would otherwise drop.
test("lite-07: an unanswered track's datagrams don't delay another track's last", async () => {
	const source = new Origin();
	const publication = source.route(path, { hidden: true });
	const b = publication.broadcast.createDatagrams("b");
	// Answers "a" on demand.
	const requested = wireOf(publication.broadcast).requestedDatagrams();
	publication.announce();
	const pair = createMockTransportPair(ALPN_07_WIP);
	const hold = holdNextAnswer(pair.server);
	// Counts the datagrams the publisher sends.
	const sending = pair.server.datagrams.writable.getWriter();
	let sent = 0;
	const sends = new Signal(0);
	Object.defineProperty(pair.server.datagrams, "writable", {
		value: new WritableStream<Uint8Array>({
			write: async (chunk) => {
				await sending.write(chunk);
				sends.set(++sent);
			},
		}),
	});
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(client).consume(path);
	const readerB = remote.datagrams("b");
	readerB.setPriority(5);
	let readerA: ReturnType<typeof remote.datagrams> | undefined;
	try {
		await readerB.info();
		while (b.priority.peek() !== 5) await b.priority.changed();
		hold.arm();
		readerA = remote.datagrams("a");
		const request = await requested;
		if (!request) throw new Error("the request never reached the handler");
		const a = request.accept({});
		for (let i = 0; i < 40; i++) a.append(Timestamp.unscaled(i), Uint8Array.of(i));
		// "a"'s datagrams reach the subscriber ahead of "b"'s.
		while (sends.peek() < 40) await sends.changed();
		b.append(Timestamp.unscaled(0n), Uint8Array.of(9));
		b.finish();
		expect((await readerB.recv())?.payload).toEqual(Uint8Array.of(9));
		expect(await readerB.recv()).toBeUndefined();
		// Those past the cap were dropped at once, without stalling "b".
		expect((await client.stats()).datagramsUnrouted).toBeGreaterThanOrEqual(8);
		// "a" stays unanswered past the host task its held datagrams wait, so they drop too.
		while (((await client.stats()).datagramsUnrouted ?? 0) < 40) await yieldNow();
		expect((await client.stats()).datagramsUnrouted).toBe(40);
		hold.release();
		await readerA.info();
	} finally {
		readerA?.close();
		readerB.close();
		remote.close();
		client.abort();
		server.abort();
		source.close();
	}
});
