import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { SessionCode, SessionError } from "../error.ts";
import { createMockTransportPair, type MockTransport, textFrame } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { wireOf } from "../wire.ts";
import { Connection, probeLevel } from "./connection.ts";
import { Fetch } from "./fetch.ts";
import { Goaway } from "./goaway.ts";
import { Group as GroupMessage } from "./group.ts";
import { ProbeLevel, Setup } from "./setup.ts";
import { DataType, StreamId } from "./stream.ts";
import {
	decodeSubscribeResponse,
	encodeSubscribeResponse,
	Subscribe,
	SubscribeEnd,
	SubscribeStart,
} from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
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

	test(`repeated SETUP parameter closes ${alpn} with PROTOCOL_VIOLATION`, async () => {
		const pair = createMockTransportPair(alpn);
		const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
		try {
			const writer = await Writer.open(pair.server, { version });
			await writer.u53(DataType.Setup);
			// A five-byte body holding two empty parameters with the same id.
			for (const v of [5, 2, 0, 0, 0, 0]) await writer.u53(v);
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

// Holds the one-second close deadline on a manual clock, so a test fires it by hand instead of waiting.
function mockDeadline() {
	const clock = new Manual();
	return {
		expire: () => clock.advance(Nano.fromMilli(Milli(1000))),
		installed: clock.install(),
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
		const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
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
			producer.appendGroup().writeFrame(textFrame("first"));
			expect(await (await track.nextGroup())?.readString()).toBe("first");

			fin.enable();
			const last = producer.appendGroup();
			last.writeFrame(textFrame("last"));
			last.finish();
			producer.finish();
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
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const server = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.server,
		version,
		publish: origin.consume(),
	});

	const group = producer.appendGroup();
	group.writeFrame(textFrame("last"));
	group.finish();

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
	producer.finish();
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

		const failure = closing.catch((error: unknown) => error);
		await deadline.expire();
		expect(await failure).toHaveProperty("message", "session close timed out");
		expect(served.closed()).toBe(true);
	} finally {
		deadline.installed.close();
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
		deadline.installed.close();
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
		deadline.installed.close();
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
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = producer.appendGroup();
	group.writeFrame(textFrame("last"));
	group.finish();
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
		deadline.installed.close();
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
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = producer.appendGroup();
	group.writeFrame(textFrame("last"));
	group.finish();
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
		producer.finish();
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
		deadline.installed.close();
		fin.release();
		client.abort();
		server.abort();
		announced.close();
		broadcast.close();
		source.close();
		destination.close();
	}
});

for (const peerClosed of [true, false]) {
	test(`driver distinguishes peer close from an open-transport failure: ${peerClosed}`, async () => {
		const pair = createMockTransportPair(ALPN_06);
		let incoming!: ReadableStreamDefaultController<WebTransportBidirectionalStream>;
		Object.defineProperty(pair.client, "incomingBidirectionalStreams", {
			value: new ReadableStream<WebTransportBidirectionalStream>({
				start(controller) {
					incoming = controller;
				},
			}),
		});
		let stopped!: () => void;
		const terminal = new Promise<void>((resolve) => {
			stopped = resolve;
		});
		const original = pair.client.close.bind(pair.client);
		const close = spyOn(pair.client, "close").mockImplementation((info) => {
			original(info);
			stopped();
		});
		const logged = spyOn(console, "error").mockImplementation(() => {});
		const connection = new Connection({
			url: new URL("https://example.com/"),
			quic: pair.client,
			version: Version.DRAFT_06,
		});
		const failure = new Error(peerClosed ? "Connection closed: 2 unauthorized" : "local accept failed");
		try {
			// QMux fulfills closed before it rejects pending accepts with its raw teardown error.
			if (peerClosed) pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
			incoming.error(failure);
			// The driver's final abort is the barrier, so the spy cannot outlive this fixture.
			await terminal;
			if (peerClosed) {
				const reason = await connection.closed;
				expect(reason).toBeInstanceOf(SessionError);
				expect((reason as SessionError).code).toBe(SessionCode.Unauthorized);
				expect(logged).not.toHaveBeenCalled();
			} else {
				expect(logged).toHaveBeenCalledWith("fatal error running connection", failure);
			}
		} finally {
			connection.abort();
			pair.server.close();
			close.mockRestore();
			logged.mockRestore();
		}
	});
}

test("a malformed complete GOAWAY closes the session", async () => {
	for (const [alpn, version] of [
		[ALPN_04, Version.DRAFT_04],
		[ALPN_05, Version.DRAFT_05],
		[ALPN_06, Version.DRAFT_06],
		[ALPN_07_WIP, Version.DRAFT_07],
	] as const) {
		const pair = createMockTransportPair(alpn);
		const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
		const close = spyOn(pair.client, "close");
		try {
			const stream = await Stream.open(pair.server, { version });
			await stream.writer.u53(StreamId.Goaway);
			await stream.writer.write(new Uint8Array([2, 0, 0]));
			await new Promise<void>((resolve) => {
				const channel = new MessageChannel();
				channel.port1.onmessage = () => {
					channel.port1.close();
					channel.port2.close();
					resolve();
				};
				channel.port2.postMessage(null);
			});
			expect(close.mock.calls.some(([info]) => info?.closeCode === SessionCode.ProtocolViolation)).toBe(true);
		} finally {
			close.mockRestore();
			connection.abort();
		}
	}
});

// Each write is backpressured until the receiver reads it, before FIN is sent.
for (const [alpn, version] of [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const) {
	test(`padding is discarded only on lite-07: ${alpn}`, async () => {
		const pair = createMockTransportPair(alpn);
		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.server,
			version,
		});
		let closed = false;
		void connection.closed.then(() => {
			closed = true;
		});
		const writer = new Writer(await pair.client.createUnidirectionalStream(), version);
		try {
			await writer.u53(2);
			if (version === Version.DRAFT_07) {
				for (let i = 0; i < 32; i++) await writer.write(new Uint8Array(4096));
				writer.close();
				await writer.closed;
			} else {
				await expect(writer.closed).rejects.toThrow();
			}
			expect(closed).toBe(false);
			// A subsequent stream is still handled after the padding finishes or resets.
			const goaway = await Stream.open(pair.client, { version });
			await goaway.writer.u53(StreamId.Goaway);
			await new Goaway("").encode(goaway.writer, version);
			goaway.writer.close();
			expect((await wireOf(connection).goaway).uri).toBe("");
		} finally {
			connection.abort();
		}
	});
}

// A group stream that breaks the protocol ends the session, so a hostile peer cannot repeat
// it on every stream.
for (const violation of ["a partial group", "a group past the declared end"] as const) {
	test(`${violation} closes the session with PROTOCOL_VIOLATION`, async () => {
		const version = Version.DRAFT_07;
		const pair = createMockTransportPair(ALPN_07_WIP);
		const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
		try {
			const track = wireOf(connection).consume(Path.from("room")).track("video").subscribe();

			// The publisher, played by hand: answer TRACK_INFO, start the subscription, and
			// leave any other stream the session opens (such as PROBE) open.
			let sub: Stream | undefined;
			while (!sub) {
				const stream = await Stream.accept(pair.server, version);
				if (!stream) throw new Error("the subscriber never subscribed");
				const id = await stream.reader.u53();
				if (id === StreamId.Track) {
					await TrackMessage.decode(stream.reader, version);
					await new TrackInfo({ maxAge: 60_000, timescale: Timescale.MILLI }).encode(stream.writer, version);
					stream.close();
				} else if (id === StreamId.Subscribe) {
					await Subscribe.decode(stream.reader, version);
					sub = stream;
				}
			}
			await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0) }, version);

			let group: GroupMessage;
			if (violation === "a partial group") {
				// The subscriber never asked to start mid-group.
				group = new GroupMessage({ subscribe: 0n, sequence: 0, frameStart: 5 });
			} else {
				await encodeSubscribeResponse(sub.writer, { end: new SubscribeEnd(2) }, version);
				group = new GroupMessage({ subscribe: 0n, sequence: 2 });
			}
			const writer = await Writer.open(pair.server, { version });
			await writer.u53(DataType.Group);
			await group.encode(writer, version);

			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
			track.close();
		} finally {
			connection.abort();
		}
	});
}
