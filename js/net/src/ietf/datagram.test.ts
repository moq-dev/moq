import { expect, spyOn, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { accept, connect } from "../connection/index.ts";
import { ProtocolViolation, SessionCode } from "../error.ts";
import { createMockTransportPair, type MockTransport, manual, settlesWithin } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { encodeVarint, Stream } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { Budget } from "../util/yield.ts";
import { wireOf } from "../wire.ts";
import { Connection } from "./connection.ts";
import { ObjectDatagram } from "./datagram.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const ALL: IetfVersion[] = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22];

function single(publisherPriority?: number): ObjectDatagram {
	return new ObjectDatagram({
		trackAlias: 3n,
		groupId: 42n,
		publisherPriority,
		endOfGroup: true,
		properties: Uint8Array.of(0x10, 0x05),
		body: { payload: new TextEncoder().encode("hello") },
	});
}

async function rejects(bytes: Uint8Array, version: IetfVersion) {
	expect(await ObjectDatagram.decode(bytes, version).catch((err: unknown) => err)).toBeInstanceOf(ProtocolViolation);
}

test("roundtrips on every draft", async () => {
	for (const version of ALL) {
		const datagram = single(7);
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x07); // properties, end of group, object 0
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("an explicit object id and a status", async () => {
	const datagram = new ObjectDatagram({
		trackAlias: 1n,
		groupId: 2n,
		objectId: 0n,
		publisherPriority: 128,
		endOfGroup: false,
		body: { status: 0 },
	});
	for (const version of ALL) {
		const bytes = datagram.encode(version);
		expect(bytes[0]).toBe(0x20);
		expect(await ObjectDatagram.decode(bytes, version)).toEqual(datagram);
	}
});

test("rejects invalid types", async () => {
	for (const version of ALL) {
		// A status that ends the group, the reserved bit, a bit past the defined ones, and a
		// Type too wide for any of them.
		for (const kind of [0x22, 0x10, 0x40, 2 ** 40]) {
			const type = encodeVarint(kind, version);
			const bytes = new Uint8Array(type.byteLength + 4);
			bytes.set(type);
			bytes.set([0x01, 0x02, 0x03, 0x04], type.byteLength);
			await rejects(bytes, version);
		}
	}
});

/** A server session, so a datagram written by the client is received. */
async function session() {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const control = await Stream.open(pair.server, { version });
	const connection = new Connection({
		url: new URL("https://example.com"),
		quic: pair.server,
		control,
		version,
		client: false,
	});
	const writable = pair.client.datagrams.writable;
	if (!writable) throw new Error("mock datagrams have no writable");
	return { pair, connection, version, writable };
}

test("an undefined object status closes the session", async () => {
	const { pair, connection, version, writable } = await session();
	try {
		const bytes = new ObjectDatagram({
			trackAlias: 1n,
			groupId: 4n,
			endOfGroup: false,
			body: { status: 1 },
		}).encode(version);
		const writer = writable.getWriter();
		await writer.write(bytes);
		const closed = await pair.server.closed;
		expect(closed.closeCode).toBe(SessionCode.ProtocolViolation);
	} finally {
		connection.abort();
	}
});

test("a defined object status does not close the session", async () => {
	using clock = manual();
	const { pair, connection, version, writable } = await session();
	try {
		const bytes = new ObjectDatagram({
			trackAlias: 1n,
			groupId: 4n,
			endOfGroup: false,
			body: { status: 0 },
		}).encode(version);
		const writer = writable.getWriter();
		await writer.write(bytes);
		expect(await settlesWithin(pair.server.closed, clock, Duration.millis(50))).toBe(false);
	} finally {
		connection.abort();
	}
});

test("a datagram track round-trips over moq-transport", async () => {
	using clock = manual();
	const source = new Origin();
	const path = Path.from("live");
	const publication = source.route(path, { hidden: true });
	const datagrams = publication.broadcast.createDatagrams("audio", {
		timescale: Timescale.MILLI,
		priority: 3,
	});
	publication.announce();
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const url = new URL("https://localhost/datagrams");
	const peers = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const remote = wireOf(peers[0]).consume(path);
	const reader = remote.datagrams("audio");
	reader.setPriority(1);
	try {
		const info = await reader.info();
		while (datagrams.priority.peek() !== 1) await datagrams.priority.changed();
		expect(info.priority).toBe(3);
		expect(info.timescale).toBe(Timescale.MILLI);
		// A peer's unreadable properties lose one best-effort payload, not the session.
		const encode = ObjectDatagram.prototype.encode;
		const corrupt = spyOn(ObjectDatagram.prototype, "encode").mockImplementation(function (
			this: ObjectDatagram,
			version,
		) {
			corrupt.mockRestore();
			return encode.call(new ObjectDatagram({ ...this, properties: Uint8Array.of(0x10) }), version);
		});
		datagrams.insert({ sequence: 3n, timestamp: Timestamp.fromMillis(8), payload: Uint8Array.of(0) });

		datagrams.insert({ sequence: 4n, timestamp: Timestamp.fromMillis(9), payload: new Uint8Array([7]) });
		const received = await reader.recv();
		expect(received?.sequence).toBe(4n);
		expect(received?.timestamp?.asMillis()).toBe(9);
		expect(received?.payload).toEqual(new Uint8Array([7]));
		datagrams.finish();
		// The end waits out the grace a late datagram gets.
		expect(await clock.drive(reader.recv())).toBeUndefined();
	} finally {
		reader.close();
		await peers[0].close();
		await peers[1].close();
		source.close();
	}
});

// Either model kind can arrive first; the relay must retain one upstream request and
// route each payload only to readers of its kind, even after one reader leaves.
test("a relay shares one IETF subscription across stream and datagram readers", async () => {
	for (const datagramFirst of [false, true]) {
		const source = new Origin();
		const relay = new Origin();
		const path = Path.from("shared");
		const publication = source.route(path, { hidden: true });
		const track = publication.broadcast.createTrack("audio", { timescale: Timescale.MILLI });
		const datagrams = publication.broadcast.createDatagrams("audio", { timescale: Timescale.MICRO });
		publication.announce();
		const upstream = createMockTransportPair(ALPN.DRAFT_18);
		const url = new URL("https://localhost/shared");
		const peers = await Promise.all([
			connect({ url, transport: upstream.client }),
			accept({ url, transport: upstream.server, publish: source.consume() }),
		]);
		const original = wireOf(peers[0]).consume(path);
		const requests = relay.claim(path);
		const cursor = requests.requested();
		const routing = (async () => {
			for await (const request of cursor) request.serve(original);
		})();
		const downstream = createMockTransportPair(ALPN.DRAFT_22);
		const viewers = await Promise.all([
			connect({ url, transport: downstream.client }),
			accept({ url, transport: downstream.server, publish: relay.consume() }),
		]);
		const remote = wireOf(viewers[0]).consume(path);
		const stream = datagramFirst ? undefined : remote.track("audio").subscribe();
		if (stream) await stream.info();
		const streamCount = stream && upstream.client.sendStreams.bidi.length;
		let reader = remote.datagrams("audio");
		await reader.info();
		const before = streamCount ?? upstream.client.sendStreams.bidi.length;
		const subscriber = stream ?? remote.track("audio").subscribe();
		await subscriber.info();
		expect(upstream.client.sendStreams.bidi.length).toBe(before);
		try {
			datagrams.insert({ sequence: 4n, timestamp: Timestamp.fromMillis(9), payload: Uint8Array.of(7) });
			const group = track.appendGroup();
			group.writeFrame({ timestamp: Timestamp.fromMillis(10), payload: Uint8Array.of(8) });
			group.finish();
			const received = await reader.recv();
			expect(received?.payload).toEqual(Uint8Array.of(7));
			expect(received?.timestamp?.asMillis()).toBe(9);
			const receivedGroup = await subscriber.recvGroup();
			expect((await receivedGroup?.readFrame())?.payload).toEqual(Uint8Array.of(8));
			receivedGroup?.close();
			subscriber.close();
			datagrams.insert({ sequence: 5n, timestamp: Timestamp.fromMillis(11), payload: Uint8Array.of(9) });
			expect((await reader.recv())?.payload).toEqual(Uint8Array.of(9));
			reader.close();
			while (track.subscription.peek() !== undefined) await track.subscription.changed();
			reader = remote.datagrams("audio");
			await reader.info();
			datagrams.insert({ sequence: 6n, timestamp: Timestamp.fromMillis(12), payload: Uint8Array.of(10) });
			expect((await reader.recv())?.payload).toEqual(Uint8Array.of(10));
		} finally {
			subscriber.close();
			reader.close();
			for (const peer of viewers) await peer.close();
			requests.close();
			await routing;
			for (const peer of peers) await peer.close();
			relay.close();
			source.close();
		}
	}
});

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

for (const alpn of [ALPN.DRAFT_18, ALPN.DRAFT_20, ALPN.DRAFT_22]) {
	for (const [lag, finish] of [
		[0, false],
		[100, false],
		[0, true],
		[100, true],
	] as const) {
		test(`${alpn}: a datagram sent right behind its answer arrives (answer ${lag} microtasks late${finish ? ", then the end" : ""})`, async () => {
			using clock = manual();
			const source = new Origin();
			const publication = source.route(Path.from("live"), { hidden: true });
			const datagrams = publication.broadcast.createDatagrams("audio");
			publication.announce();
			const pair = createMockTransportPair(alpn);
			lagAnswers(pair.server, lag);
			const [client, server] = await Promise.all([
				connect({ url: new URL("https://localhost/datagrams"), transport: pair.client }),
				accept({
					url: new URL("https://localhost/datagrams"),
					transport: pair.server,
					publish: source.consume(),
				}),
			]);
			const remote = wireOf(client).consume(Path.from("live"));
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
				// The end waits out the grace a late datagram gets.
				if (finish) expect(await clock.drive(reader.recv())).toBeUndefined();
				else expect((await reader.recv())?.payload).toEqual(Uint8Array.of(2));
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

for (const alpn of [ALPN.DRAFT_18, ALPN.DRAFT_20, ALPN.DRAFT_22]) {
	test(`${alpn}: a datagram burst reaches its reader before the end across a receive yield`, async () => {
		using clock = manual();
		const source = new Origin();
		const publication = source.route(Path.from("live"), { hidden: true });
		const datagrams = publication.broadcast.createDatagrams("audio");
		publication.announce();
		const pair = createMockTransportPair(alpn);
		const url = new URL("https://localhost/datagrams");
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: source.consume() }),
		]);
		const remote = wireOf(client).consume(Path.from("live"));
		const reader = remote.datagrams("audio");
		reader.setPriority(5);
		let polls = 0;
		const budget = spyOn(Budget.prototype, "poll").mockImplementation(() =>
			++polls % 32 === 0 ? yieldNow() : undefined,
		);
		try {
			await reader.info();
			while (datagrams.priority.peek() !== 5) await datagrams.priority.changed();
			for (let i = 0; i < 64; i++) datagrams.append(Timestamp.unscaled(i), Uint8Array.of(i));
			datagrams.finish();
			for (let i = 0; i < 64; i++) expect((await reader.recv())?.payload).toEqual(Uint8Array.of(i));
			// The end waits out the grace a late datagram gets.
			expect(await clock.drive(reader.recv())).toBeUndefined();
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
