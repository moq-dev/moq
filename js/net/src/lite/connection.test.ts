import { expect, spyOn, test } from "bun:test";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { SessionCode } from "../error.ts";
import { createMockTransportPair, type MockTransport } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream, Writer } from "../stream.ts";
import { wireOf } from "../wire.ts";
import { Connection, probeLevel } from "./connection.ts";
import { Fetch } from "./fetch.ts";
import { Goaway } from "./goaway.ts";
import { ProbeLevel, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import { decodeSubscribeResponse, Subscribe } from "./subscribe.ts";
import { ALPN_04, ALPN_05, ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

/** A transport whose `getStats` behaves as described, or is absent entirely. */
function transport(getStats?: () => Promise<unknown>): WebTransport {
	return (getStats ? { getStats } : {}) as unknown as WebTransport;
}

// `Report` claims we can measure and periodically report. The qmux/WebSocket
// fallback implements no `getStats()`, so a publisher there has nothing to send;
// advertising Report and then holding the subscriber's PROBE stream open with
// nothing on it is the state this avoids.
test("no getStats advertises None", async () => {
	expect(await probeLevel(transport(), Version.DRAFT_05)).toBe(ProbeLevel.None);
});

// Having the method is not the same as having a measurement.
test("getStats with no usable metric advertises None", async () => {
	const quic = transport(async () => ({ estimatedSendRate: null }));
	expect(await probeLevel(quic, Version.DRAFT_05)).toBe(ProbeLevel.None);
});

test("either metric alone is enough to advertise Report", async () => {
	const rateOnly = transport(async () => ({ estimatedSendRate: 1_000_000 }));
	expect(await probeLevel(rateOnly, Version.DRAFT_05)).toBe(ProbeLevel.Report);

	const rttOnly = transport(async () => ({ estimatedSendRate: null, smoothedRtt: 12.34 }));
	expect(await probeLevel(rttOnly, Version.DRAFT_05)).toBe(ProbeLevel.Report);
});

// lite-03's PROBE has no RTT field, so an RTT is not something we could report
// there even though we can measure it.
test("an RTT alone is not reportable on a version that cannot carry one", async () => {
	const rttOnly = transport(async () => ({ estimatedSendRate: null, smoothedRtt: 12.34 }));
	expect(await probeLevel(rttOnly, Version.DRAFT_03)).toBe(ProbeLevel.None);
});

// A transport that cannot answer tells us nothing, which is itself an answer. A
// throwing getStats must not escape into the SETUP path.
test("a throwing getStats advertises None rather than propagating", async () => {
	const quic = transport(async () => {
		throw new Error("no stats for you");
	});
	expect(await probeLevel(quic, Version.DRAFT_05)).toBe(ProbeLevel.None);
});

async function sendGoaway(server: WebTransport, uri: string): Promise<void> {
	const stream = await Stream.open(server, { version: Version.DRAFT_04 });
	await stream.writer.u53(StreamId.Goaway);
	await new Goaway(uri).encode(stream.writer, Version.DRAFT_04);
	stream.writer.close();
}

test("a lite GOAWAY keeps the session open, and a second one closes it", async () => {
	const pair = createMockTransportPair(ALPN_04);
	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		version: Version.DRAFT_04,
	});

	let closed = false;
	void connection.closed.then(() => {
		closed = true;
	});

	try {
		await sendGoaway(pair.server, "");
		const drain = await wireOf(connection).goaway;
		expect(drain.uri).toBe("");

		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(closed).toBe(false);

		await sendGoaway(pair.server, "https://other.example/");
		await new Promise((resolve) => setTimeout(resolve, 50));
		expect(closed).toBe(true);
	} finally {
		connection.abort();
	}
});

for (const [alpn, version] of [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const) {
	for (const complete of [false, true]) {
		test(`duplicate SETUP closes ${alpn} with PROTOCOL_VIOLATION (complete=${complete})`, async () => {
			const pair = createMockTransportPair(alpn);
			const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
			try {
				for (let i = 0; i < 2; i++) {
					const writer = await Writer.open(pair.server, { version });
					await writer.u53(DataType.Setup);
					if (complete && i === 0) {
						await new Setup({}).encode(writer, version);
						writer.close();
					}
				}
				expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			} finally {
				connection.abort();
			}
		});
	}

	// The Setup Stream is claimed before its body decodes, so a truncated one leaves
	// nothing for SETUP-gated streams to wait on.
	test(`truncated SETUP closes ${alpn} with PROTOCOL_VIOLATION`, async () => {
		const pair = createMockTransportPair(alpn);
		const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
		try {
			const writer = await Writer.open(pair.server, { version });
			await writer.u53(DataType.Setup);
			writer.close();
			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			connection.abort();
		}
	});
}

// Once enabled, hold the FIN acknowledgement of every stream `transport` writes on (unidirectional
// streams it opens, or bidirectional streams the peer opens) after the bytes and FIN reached the
// peer: a group stream or withdrawal whose tail is still in flight.
function holdFins(transport: MockTransport, streams: "uni" | "incoming-bidi") {
	const reached = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let enabled = false;
	const wrap = (writable: WritableStream<Uint8Array>) => {
		const writer = writable.getWriter();
		return new WritableStream<Uint8Array>({
			write: (bytes) => writer.write(bytes),
			abort: (reason) => writer.abort(reason),
			async close() {
				await writer.close();
				if (!enabled) return;
				reached.resolve();
				await release.promise;
			},
		});
	};
	if (streams === "uni") {
		const create = transport.createUnidirectionalStream.bind(transport);
		transport.createUnidirectionalStream = async (options) => wrap(await create(options));
	} else {
		Object.defineProperty(transport, "incomingBidirectionalStreams", {
			value: transport.incomingBidirectionalStreams.pipeThrough(
				new TransformStream<WebTransportBidirectionalStream, WebTransportBidirectionalStream>({
					transform(stream, controller) {
						controller.enqueue({ readable: stream.readable, writable: wrap(stream.writable) });
					},
				}),
			),
		});
	}
	return {
		reached: reached.promise,
		enable: () => {
			enabled = true;
		},
		release: release.resolve,
	};
}

// Captures the one-second close deadline so a test fires it by hand instead of waiting.
function mockDeadline() {
	let expire: (() => void) | undefined;
	const original = globalThis.setTimeout;
	const timer = spyOn(globalThis, "setTimeout").mockImplementation(((fn: () => void, ms?: number) => {
		if (ms !== 1000) return original(fn, ms);
		expire = fn;
		return 0 as unknown as ReturnType<typeof setTimeout>;
	}) as typeof setTimeout);
	return {
		expire: () => {
			if (!expire) throw new Error("close armed no deadline");
			expire();
		},
		restore: () => timer.mockRestore(),
	};
}

/** Runs every task queued so far, so a test can assert that something is still pending. */
async function settle() {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

for (const alpn of [ALPN_05, ALPN_06, ALPN_07_WIP]) {
	// A publisher that finishes a track and closes must not cut the final group short.
	test(`close delivers a finished track's final group over ${alpn}`, async () => {
		const pair = createMockTransportPair(alpn);
		const fin = holdFins(pair.server, "uni");
		const origin = new Producer();
		const broadcast = origin.createBroadcast(Path.from("room"));
		broadcast.announce();
		const producer = broadcast.createTrack("video");
		const url = new URL("https://localhost/test");
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ url, transport: pair.server, publish: origin.consume() }),
		]);
		const remote = wireOf(client).consume(Path.from("room"));
		const track = remote.track("video").subscribe().ordered();
		let closed = false;
		void pair.server.closed.then(() => {
			closed = true;
		});
		try {
			producer.appendGroup().writeString("first");
			expect(await (await track.nextGroup())?.readString()).toBe("first");

			fin.enable();
			const last = producer.appendGroup();
			last.writeString("last");
			last.close();
			producer.close();
			const closing = server.close();

			expect(await (await track.nextGroup())?.readString()).toBe("last");
			await fin.reached;
			await settle();
			expect(closed).toBe(false);

			fin.release();
			await closing;
			expect(closed).toBe(true);
		} finally {
			fin.release();
			track.close();
			remote.close();
			client.abort();
			server.abort();
			broadcast.close();
			origin.close();
		}
	});
}

/** A lite-07 publisher serving a finished track to a raw subscriber that has read up to the FIN. */
async function servedRawSubscription() {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new Producer();
	const broadcast = origin.createBroadcast(Path.from("room"));
	broadcast.announce();
	const producer = broadcast.createTrack("video");
	const server = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.server,
		version,
		publish: origin.consume(),
	});

	const group = producer.appendGroup();
	group.writeString("last");
	group.close();

	const subscriber = await Stream.open(pair.client, { version });
	await subscriber.writer.u53(StreamId.Subscribe);
	await new Subscribe({
		id: 0n,
		broadcast: Path.from("room"),
		track: "video",
		priority: 0,
		maxDelay: 10_000,
		startGroup: 0,
	}).encode(subscriber.writer, version);

	// Finish the track only once it is served: a track closed before anyone subscribed is gone.
	expect("start" in (await decodeSubscribeResponse(subscriber.reader, version))).toBe(true);
	producer.close();
	// SUBSCRIBE_END, then the publisher's FIN.
	await subscriber.reader.readAll();

	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});
	return {
		server,
		subscriber,
		closed: () => closed,
		cleanup: () => {
			server.abort();
			broadcast.close();
			origin.close();
		},
	};
}

// The publisher's FIN says nothing about whether the subscriber read the tail; on lite-07 the
// subscriber's FIN does, so close waits for it.
test("a lite-07 subscriber withholding FIN holds close until the deadline", async () => {
	const deadline = mockDeadline();
	const served = await servedRawSubscription();
	try {
		const closing = served.server.close();
		await settle();
		expect(served.closed()).toBe(false);

		deadline.expire();
		await expect(closing).rejects.toThrow("session close timed out");
		expect(served.closed()).toBe(true);
	} finally {
		deadline.restore();
		served.cleanup();
	}
});

test("a lite-07 subscriber FIN releases close", async () => {
	const deadline = mockDeadline();
	const served = await servedRawSubscription();
	try {
		const closing = served.server.close();
		await settle();
		expect(served.closed()).toBe(false);

		served.subscriber.writer.close();
		await closing;
		expect(served.closed()).toBe(true);
	} finally {
		deadline.restore();
		served.cleanup();
	}
});

test("abort during the close drain ends the session at once", async () => {
	const deadline = mockDeadline();
	const served = await servedRawSubscription();
	try {
		const closing = served.server.close();
		await settle();
		expect(served.closed()).toBe(false);

		served.server.abort();
		await closing;
		expect(served.closed()).toBe(true);
	} finally {
		deadline.restore();
		served.cleanup();
	}
});

// A served FETCH stays owed until the peer acknowledges its FIN, so close cannot discard its tail.
test("close waits for a served FETCH to be acknowledged", async () => {
	const deadline = mockDeadline();
	const version = Version.DRAFT_05;
	const pair = createMockTransportPair(ALPN_05);
	const fin = holdFins(pair.server, "incoming-bidi");
	const origin = new Producer();
	const broadcast = origin.createBroadcast(Path.from("room"));
	broadcast.announce();
	const producer = broadcast.createTrack("video");
	const group = producer.appendGroup();
	group.writeString("last");
	group.close();
	const server = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.server,
		version,
		publish: origin.consume(),
	});
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});
	try {
		fin.enable();
		const fetch = await Stream.open(pair.client, { version });
		await fetch.writer.u53(StreamId.Fetch);
		await new Fetch({ broadcast: Path.from("room"), track: "video", priority: 0, group: 0 }).encode(
			fetch.writer,
			version,
		);
		await fetch.reader.readAll();
		await fin.reached;

		const closing = server.close();
		await settle();
		expect(closed).toBe(false);

		fin.release();
		await closing;
		expect(closed).toBe(true);
	} finally {
		deadline.restore();
		fin.release();
		server.abort();
		broadcast.close();
		origin.close();
	}
});

// A request that arrives while the withdrawals are still in flight is owed too, even though
// nothing was owed when close began.
test("close waits for a request served while withdrawals are in flight", async () => {
	const deadline = mockDeadline();
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const fin = holdFins(pair.server, "incoming-bidi");
	const source = new Producer();
	const destination = new Producer();
	const broadcast = source.createBroadcast(Path.from("room"));
	broadcast.announce();
	const producer = broadcast.createTrack("video");
	const group = producer.appendGroup();
	group.writeString("last");
	group.close();
	const url = new URL("https://localhost/test");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume: destination }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const announced = destination.announced();
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});
	try {
		expect((await announced.next())?.kind).toBe("start");

		fin.enable();
		const closing = server.close();
		await fin.reached;

		const subscriber = await Stream.open(pair.client, { version });
		await subscriber.writer.u53(StreamId.Subscribe);
		await new Subscribe({
			id: 0n,
			broadcast: Path.from("room"),
			track: "video",
			priority: 0,
			maxDelay: 10_000,
			startGroup: 0,
		}).encode(subscriber.writer, version);
		expect("start" in (await decodeSubscribeResponse(subscriber.reader, version))).toBe(true);
		producer.close();
		await subscriber.reader.readAll();

		// The withdrawal completes, but the subscription still waits for the subscriber's FIN.
		fin.release();
		await settle();
		expect(closed).toBe(false);

		subscriber.writer.close();
		await closing;
		expect(closed).toBe(true);
	} finally {
		deadline.restore();
		fin.release();
		client.abort();
		server.abort();
		announced.close();
		broadcast.close();
		source.close();
		destination.close();
	}
});
