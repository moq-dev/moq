import { expect, spyOn, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Session as Qmux } from "@moq/qmux";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { inspect } from "../connection/stats.ts";
import { defined } from "../defined.test-util.ts";
import { SessionCode, SessionError } from "../error.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair, type MockTransport, manual, settlesWithin, textFrame } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { acceptBudget, Stream, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { wireOf } from "../wire.ts";
import { type AnnounceBroadcast, encodeAnnounceBroadcast } from "./announce.ts";
import { AnnounceStream } from "./announce_stream.ts";
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
import {
	ALPN,
	ALPN_03,
	ALPN_04,
	ALPN_05,
	ALPN_06,
	ALPN_07_WIP,
	hasRanges,
	hasTrackStream,
	keepsSubscribeOpen,
	Version,
	versionName,
} from "./version.ts";

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

// Safari's stub reports a 0 RTT and no rate; once inspect() flags it, the wire says None.
test("a stub getStats found at setup advertises None", async () => {
	const stub = transport(async () => ({ smoothedRtt: 0, estimatedSendRate: null }));
	await inspect(stub);
	expect(await probeLevel(stub, Version.DRAFT_05)).toBe(ProbeLevel.None);

	// A 0 RTT alongside a rate is not a stub.
	const rate = transport(async () => ({ smoothedRtt: 0, estimatedSendRate: 1_000_000 }));
	await inspect(rate);
	expect(await probeLevel(rate, Version.DRAFT_05)).toBe(ProbeLevel.Report);
});

async function sendGoaway(server: WebTransport, uri: string): Promise<void> {
	const stream = await Stream.open(server, { version: Version.DRAFT_04 });
	await stream.writer.u53(StreamId.Goaway);
	await new Goaway(uri).encode(stream.writer, Version.DRAFT_04);
	stream.writer.close();
}

test("a lite GOAWAY keeps the session open, and a second one closes it", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_04);
	const connection = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		version: Version.DRAFT_04,
	});

	try {
		await sendGoaway(pair.server, "");
		const drain = await connection.draining;
		expect(drain.uri).toBe("");
		expect(await settlesWithin(connection.closed, clock, Duration.millis(20))).toBe(false);

		await sendGoaway(pair.server, "https://other.example/");
		expect(await settlesWithin(connection.closed, clock, Duration.millis(50))).toBe(true);
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

// Once enabled, every write on a bidirectional stream the peer opens reaches the peer before
// the writer resumes a host task later, as when the write budget yields: a reply delivered
// while its writer still waits.
function yieldReplies(transport: MockTransport) {
	let enabled = false;
	const wrap = (writable: WritableStream<Uint8Array>) => {
		const writer = writable.getWriter();
		return new WritableStream<Uint8Array>({
			async write(bytes) {
				await writer.write(bytes);
				if (enabled) await yieldNow();
			},
			abort: (reason) => writer.abort(reason),
			close: () => writer.close(),
		});
	};
	Object.defineProperty(transport, "incomingBidirectionalStreams", {
		value: transport.incomingBidirectionalStreams.pipeThrough(
			new TransformStream<WebTransportBidirectionalStream, WebTransportBidirectionalStream>({
				transform(stream, controller) {
					controller.enqueue({ readable: stream.readable, writable: wrap(stream.writable) });
				},
			}),
		),
	});
	return {
		enable: () => {
			enabled = true;
		},
	};
}

// Buffer incoming unidirectional streams outside the transport, independently of when the
// connection accepts them: its accept budget may yield until after the session closes. QMux
// keeps a finished stream's unread bytes across a close, so the reads still owed land afterwards.
function deliverWhole(transport: MockTransport) {
	const incoming = transport.incomingUnidirectionalStreams;
	Object.defineProperty(transport, "incomingUnidirectionalStreams", {
		value: new ReadableStream<ReadableStream<Uint8Array>>({
			async start(controller) {
				for await (const stream of incoming) {
					controller.enqueue(
						new ReadableStream<Uint8Array>({
							async start(whole) {
								const chunks = await Array.fromAsync(stream);
								for (const chunk of chunks) whole.enqueue(chunk);
								whole.close();
							},
						}),
					);
				}
				controller.close();
			},
		}),
	});
}

// Deliver the bytes of every bidirectional stream `transport` opens at once, but each FIN only a task
// after the session closed, so every reaction to the close runs before a SUBSCRIBE FIN sent first.
function finAfterClose(transport: MockTransport) {
	const create = transport.createBidirectionalStream.bind(transport);
	transport.createBidirectionalStream = async (options) => {
		const stream = await create(options);
		const readable = stream.readable.pipeThrough(
			new TransformStream<Uint8Array, Uint8Array>({
				async flush() {
					await transport.closed;
					await yieldNow();
				},
			}),
		);
		return { readable, writable: stream.writable };
	};
}

// A qmux session pair over an in-memory socket. Unlike the mock, its receive streams are
// pull-driven, so a stream's FIN surfaces only to a read.
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

// Holds the one-second close deadline on a manual clock, so a test fires it by hand instead of waiting.
function mockDeadline() {
	const clock = new Manual();
	return {
		expire: () => clock.advance(Duration.millis(1000)),
		installed: clock.install(),
	};
}

/** Runs every task queued so far, so a test can assert that something is still pending. */
async function settle() {
	for (let i = 0; i < 5; i++) await yieldNow();
}

for (const [alpn, version] of [
	[ALPN, Version.DRAFT_01],
	[ALPN, Version.DRAFT_02],
	[ALPN_03, Version.DRAFT_03],
	[ALPN_04, Version.DRAFT_04],
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const) {
	// A publisher that finishes a track and closes must not cut the final group short.
	test(`close delivers a finished track's final group over ${versionName(version)}`, async () => {
		const pair = createMockTransportPair(alpn);
		const fin = holdFins(pair.server, "uni");
		const origin = new Producer();
		const broadcastRoute = origin.route(Path.from("room"));
		const broadcast = broadcastRoute.broadcast;
		const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
		const url = new URL("https://localhost/test");
		const client = new Connection({ url, quic: pair.client, version });
		const server = new Connection({ url, quic: pair.server, version, publish: origin.consume(), client: false });
		const remote = wireOf(client).consume(Path.from("room"));
		const track = remote.track("video").subscribe().ordered();
		let closed = false;
		void pair.server.closed.then(() => {
			closed = true;
		});
		try {
			const first = hooks.appendNext(producer);
			first.writeFrame(textFrame("first", Timestamp.fromMillis(Number(first.start))));
			first.finish();
			// A frame can arrive while its sender still yields on the write. Drain the setup group
			// before a zero-delay subscription supersedes its still-unfinished sender.
			const received = await track.nextGroup();
			expect(await received?.readString()).toBe("first");
			expect(await received?.readString()).toBeUndefined();
			received?.close();

			fin.enable();
			const last = hooks.appendNext(producer);
			last.writeFrame(textFrame("last", Timestamp.fromMillis(Number(last.start))));
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
			expect(await track.nextGroup()).toBeUndefined();
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

	// A lite-07 subscriber FINs once it read the tail, which can land while SUBSCRIBE_END's
	// write still waits; the final group's FIN still holds the close.
	if (keepsSubscribeOpen(version)) {
		test(`a subscriber FIN during the end's write still drains the final group over ${versionName(version)}`, async () => {
			const pair = createMockTransportPair(alpn);
			const fin = holdFins(pair.server, "uni");
			const replies = yieldReplies(pair.server);
			const origin = new Producer();
			const broadcastRoute = origin.route(Path.from("room"));
			const broadcast = broadcastRoute.broadcast;
			const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
			const url = new URL("https://localhost/test");
			const client = new Connection({ url, quic: pair.client, version });
			const server = new Connection({
				url,
				quic: pair.server,
				version,
				publish: origin.consume(),
				client: false,
			});
			const remote = wireOf(client).consume(Path.from("room"));
			const track = remote.track("video").subscribe().ordered();
			let closed = false;
			void pair.server.closed.then(() => {
				closed = true;
			});
			try {
				const first = hooks.appendNext(producer);
				first.writeFrame(textFrame("first", Timestamp.fromMillis(Number(first.start))));
				first.finish();
				const received = await track.nextGroup();
				expect(await received?.readString()).toBe("first");
				expect(await received?.readString()).toBeUndefined();
				received?.close();

				fin.enable();
				const last = hooks.appendNext(producer);
				last.writeFrame(textFrame("last", Timestamp.fromMillis(Number(last.start))));
				last.finish();
				expect(await (await track.nextGroup())?.readString()).toBe("last");
				await fin.reached;
				replies.enable();
				producer.finish();
				producer.close();
				const closing = server.close();

				expect(await track.nextGroup()).toBeUndefined();
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

	// The peer's close can overtake the final group or, where it is the track's only end, the
	// SUBSCRIBE FIN it sent first; the closing session reads them.
	for (const overtaken of hasTrackStream(version) ? ["group"] : ["group", "fin"]) {
		test(`a peer close drains the overtaken ${overtaken} over ${versionName(version)}`, async () => {
			// Exercise a close that overtakes acceptance, regardless of the host CPU budget.
			const yielding = spyOn(acceptBudget, "poll").mockImplementation(() => yieldNow());
			const pair = createMockTransportPair(alpn);
			deliverWhole(pair.client);
			if (overtaken === "fin") finAfterClose(pair.client);
			const origin = new Producer();
			const broadcastRoute = origin.route(Path.from("room"));
			const broadcast = broadcastRoute.broadcast;
			const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
			const url = new URL("https://localhost/test");
			const client = new Connection({ url, quic: pair.client, version });
			const server = new Connection({
				url,
				quic: pair.server,
				version,
				publish: origin.consume(),
				client: false,
			});
			const remote = wireOf(client).consume(Path.from("room"));
			const track = remote.track("video").subscribe().ordered();
			try {
				const first = hooks.appendNext(producer);
				first.writeFrame(textFrame("first", Timestamp.fromMillis(Number(first.start))));
				first.finish();
				expect(await (await track.nextGroup())?.readString()).toBe("first");

				const frames = Array.from({ length: 32 }, (_, i) => `frame ${i}`);
				const last = hooks.appendNext(producer);
				for (const frame of frames) last.writeFrame(textFrame(frame));
				last.finish();
				producer.finish();
				producer.close();
				await server.close();

				const group = await track.nextGroup();
				for (const frame of frames) expect(await group?.readString()).toBe(frame);
				expect(await group?.readString()).toBeUndefined();
				expect(await track.nextGroup()).toBeUndefined();
			} finally {
				yielding.mockRestore();
				track.close();
				remote.close();
				client.abort();
				server.abort();
				broadcast.close();
				origin.close();
			}
		});
	}

	// A local close reads what already arrived as the peer's close does: a track whose
	// SUBSCRIBE FIN is in flight, its only end on legacy drafts, still finishes.
	if (!hasTrackStream(version)) {
		test(`a local close drains a FIN sent first over ${versionName(version)}`, async () => {
			const pair = createMockTransportPair(alpn);
			finAfterClose(pair.client);
			const origin = new Producer();
			const broadcastRoute = origin.route(Path.from("room"));
			const broadcast = broadcastRoute.broadcast;
			const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
			const url = new URL("https://localhost/test");
			const client = new Connection({ url, quic: pair.client, version });
			const server = new Connection({
				url,
				quic: pair.server,
				version,
				publish: origin.consume(),
				client: false,
			});
			const remote = wireOf(client).consume(Path.from("room"));
			const track = remote.track("video").subscribe().ordered();
			try {
				const group = hooks.appendNext(producer);
				group.writeFrame(textFrame("only", Timestamp.fromMillis(Number(group.start))));
				group.finish();
				expect(await (await track.nextGroup())?.readString()).toBe("only");

				producer.finish();
				producer.close();
				await settle();
				client.abort();

				expect(await track.nextGroup()).toBeUndefined();
				expect(await track.closed).toBeNull();
			} finally {
				track.close();
				remote.close();
				client.abort();
				server.abort();
				broadcast.close();
				origin.close();
			}
		});
	}

	// The subscriber reads a legacy subscribe stream to its FIN, since QMux closes a stream only
	// on a read; awaiting the FIN instead would hold the closing session's drain forever.
	if (!hasTrackStream(version)) {
		test(`a peer close finishes a FIN'd track over qmux ${versionName(version)}`, async () => {
			const pair = qmuxPair();
			const origin = new Producer();
			const broadcastRoute = origin.route(Path.from("room"));
			const broadcast = broadcastRoute.broadcast;
			const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
			const url = new URL("https://localhost/test");
			const client = new Connection({ url, quic: pair.client, version });
			const server = new Connection({
				url,
				quic: pair.server,
				version,
				publish: origin.consume(),
				client: false,
			});
			const remote = wireOf(client).consume(Path.from("room"));
			const track = remote.track("video").subscribe().ordered();
			try {
				const group = hooks.appendNext(producer);
				group.writeFrame(textFrame("only", Timestamp.fromMillis(Number(group.start))));
				group.finish();
				expect(await (await track.nextGroup())?.readString()).toBe("only");

				producer.finish();
				producer.close();
				await server.close();
				expect(await track.nextGroup()).toBeUndefined();
			} finally {
				track.close();
				remote.close();
				client.abort();
				server.abort();
				broadcast.close();
				origin.close();
			}
		});
	}
}

/** A publisher serving a finished track to a raw subscriber that has read up to the FIN. */
async function servedRawSubscription(alpn: string = ALPN_07_WIP, version: Version = Version.DRAFT_07) {
	const pair = createMockTransportPair(alpn);
	const origin = new Producer();
	const broadcastRoute = origin.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	const server = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.server,
		version,
		publish: origin.consume(),
	});

	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame("last", Timestamp.fromMillis(Number(group.start))));
	group.finish();

	const subscriber = await Stream.open(pair.client, { version });
	await subscriber.writer.u53(StreamId.Subscribe);
	await new Subscribe({
		id: 0n,
		broadcast: Path.from("room"),
		track: "video",
		priority: 0,
		maxDelay: 10_000,
		startGroup: 0n,
	}).encode(subscriber.writer, version);

	// Finish the track only once it is served: a track closed before anyone subscribed is gone.
	expect("start" in (await decodeSubscribeResponse(subscriber.reader, version))).toBe(true);
	producer.finish();
	producer.close();
	// SUBSCRIBE_END, then before lite-07 the publisher's FIN; lite-07 keeps the stream open.
	if (keepsSubscribeOpen(version)) {
		while (!("end" in (await decodeSubscribeResponse(subscriber.reader, version)))) {}
	} else await subscriber.reader.readAll();

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

// The publisher's FIN says nothing about whether the subscriber read the tail, and a receiver
// may drop unread bytes on close. From lite-05, whose SUBSCRIBE_END lets the subscriber settle
// its tail at once, the subscriber's FIN says so, and close waits for it.
for (const [alpn, version] of [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const) {
	test(`a ${versionName(version)} subscriber withholding FIN holds close until the deadline`, async () => {
		const deadline = mockDeadline();
		const served = await servedRawSubscription(alpn, version);
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

	test(`a ${versionName(version)} subscriber FIN releases close`, async () => {
		const deadline = mockDeadline();
		const served = await servedRawSubscription(alpn, version);
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
}

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
	const broadcastRoute = origin.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame("last", Timestamp.fromMillis(Number(group.start))));
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
		await new Fetch({ broadcast: Path.from("room"), track: "video", priority: 0, group: 0n }).encode(
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
	// Lite-07 withdraws on its unidirectional Announce Stream.
	const fin = holdFins(pair.server, "uni");
	const source = new Producer();
	const destination = new Producer();
	const broadcastRoute = source.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
	const producer = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame("last", Timestamp.fromMillis(Number(group.start))));
	group.finish();
	const url = new URL("https://localhost/test");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume: destination }),
		accept({ url, transport: pair.server, publish: source.consume() }),
	]);
	const announced = destination.routes();
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
			startGroup: 0n,
		}).encode(subscriber.writer, version);
		expect("start" in (await decodeSubscribeResponse(subscriber.reader, version))).toBe(true);
		producer.finish();
		producer.close();
		// SUBSCRIBE_END; lite-07 keeps the stream open past it.
		while (!("end" in (await decodeSubscribeResponse(subscriber.reader, version)))) {}

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

// A stream type the version doesn't define, such as lite-07's removed padding (0x2), is
// stopped without ending the session.
for (const [alpn, version] of [
	[ALPN_05, Version.DRAFT_05],
	[ALPN_06, Version.DRAFT_06],
	[ALPN_07_WIP, Version.DRAFT_07],
] as const) {
	test(`an unknown stream type is stopped: ${alpn}`, async () => {
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
			await expect(writer.closed).rejects.toThrow();
			expect(closed).toBe(false);
			// A subsequent stream is still handled.
			const goaway = await Stream.open(pair.client, { version });
			await goaway.writer.u53(StreamId.Goaway);
			await new Goaway("").encode(goaway.writer, version);
			goaway.writer.close();
			expect((await connection.draining).uri).toBe("");
		} finally {
			connection.abort();
		}
	});
}

// A lite-06 transport tail keeps its absolute frame numbers and the session open (lite-07 has no
// Frame Start). A group past the declared end still ends the session, so a hostile peer cannot
// repeat that violation.
for (const violation of ["a partial group", "a group past the declared end"] as const) {
	test(
		violation === "a partial group"
			? "a partial group keeps its indices and the session open"
			: "a group past the declared end closes the session with PROTOCOL_VIOLATION",
		async () => {
			const version = violation === "a partial group" ? Version.DRAFT_06 : Version.DRAFT_07;
			const pair = createMockTransportPair(violation === "a partial group" ? ALPN_06 : ALPN_07_WIP);
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
						await new TrackInfo({ maxAge: 60_000, timescale: Timescale.MILLI }).encode(
							stream.writer,
							version,
						);
						stream.close();
					} else if (id === StreamId.Subscribe) {
						await Subscribe.decode(stream.reader, version);
						sub = stream;
					}
				}
				await encodeSubscribeResponse(
					sub.writer,
					{
						start: new SubscribeStart({
							id: 0n,
							start: 0n,
							largest: hasRanges(version) ? "empty" : "unknown",
						}),
					},
					version,
				);

				let group: GroupMessage;
				if (violation === "a partial group") {
					// A received tail uses its wire position even without a locally retained prefix.
					group = new GroupMessage({ subscribe: 0n, key: 0n, frameStart: 5 });
				} else {
					await encodeSubscribeResponse(sub.writer, { end: new SubscribeEnd(2n) }, version);
					group = new GroupMessage({ subscribe: 0n, key: 2n });
				}
				const writer = await Writer.open(pair.server, { version });
				await writer.u53(DataType.Group);
				await group.encode(writer, version);

				if (violation === "a partial group") {
					await writer.u62(2000n); // timestamp 1000, zigzag delta from 0
					await writer.u53(1); // a one-byte payload
					await writer.write(new Uint8Array([7]));
					writer.close();
					const received = await track.recvGroup();
					expect((await received?.readFrameIndex())?.index).toBe(5);
					expect(await received?.readFrameIndex()).toBeUndefined();
					received?.close();
					const goaway = await Stream.open(pair.server, { version });
					await goaway.writer.u53(StreamId.Goaway);
					await new Goaway("").encode(goaway.writer, version);
					goaway.writer.close();
					expect((await connection.draining).uri).toBe("");
				} else {
					expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
				}
				track.close();
			} finally {
				connection.abort();
			}
		},
	);
}

// Lite-07: each endpoint sends everything it says about discovery and routing on one
// unidirectional Announce Stream, which the peer may only finish.
async function announceStream(pair: { server: MockTransport }) {
	const writer = await Writer.open(pair.server, { version: Version.DRAFT_07 });
	await writer.u53(DataType.Announce);
	const send = (msg: AnnounceBroadcast) => encodeAnnounceBroadcast(writer, msg, Version.DRAFT_07);
	return { writer, send };
}

const ROOM = Path.from("room");

for (const [name, violate] of [
	[
		"a second Announce Stream",
		async (pair) => {
			await announceStream(pair);
			await announceStream(pair);
		},
	],
	[
		"an Announce Stream finished inside a message",
		async (pair) => {
			const { writer } = await announceStream(pair);
			// An ANNOUNCE_REQUEST of five bytes, of which one arrives.
			await writer.write(new Uint8Array([16, 5, 0]));
			writer.close();
		},
	],
	[
		"an ANNOUNCE_REQUEST nesting one prefix below another",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({
				status: "request",
				more: false,
				remove: [],
				add: [
					{ prefix: ROOM, hidden: false },
					{ prefix: Path.from("room/cam"), hidden: false },
				],
			});
		},
	],
	[
		"an ANNOUNCE_REQUEST adding a prefix below one already asked for",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({ status: "request", more: false, remove: [], add: [{ prefix: ROOM, hidden: false }] });
			await send({
				status: "request",
				more: false,
				remove: [],
				add: [{ prefix: Path.from("room/cam"), hidden: false }],
			});
		},
	],
	[
		"an ANNOUNCE_REQUEST removing an unknown Prefix ID",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({ status: "request", more: false, remove: [3n], add: [] });
		},
	],
	[
		"a message inside an edit",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({ status: "request", more: true, remove: [], add: [{ prefix: ROOM, hidden: false }] });
			await send({ status: "ok" });
		},
	],
	[
		"the parts of an edit nesting one prefix below another",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({ status: "request", more: true, remove: [], add: [{ prefix: ROOM, hidden: false }] });
			await send({
				status: "request",
				more: false,
				remove: [],
				add: [{ prefix: Path.from("room/cam"), hidden: false }],
			});
		},
	],
	[
		"an ANNOUNCE_OK answering no ANNOUNCE_REQUEST",
		async (pair) => {
			const { send } = await announceStream(pair);
			await send({ status: "ok" });
		},
	],
] as [string, (pair: { server: MockTransport }) => Promise<void>][]) {
	test(`${name} closes moq-lite-07 with PROTOCOL_VIOLATION`, async () => {
		const pair = createMockTransportPair(ALPN_07_WIP);
		const origin = new Producer();
		const connection = new Connection({
			url: new URL("https://relay.example/"),
			quic: pair.client,
			version: Version.DRAFT_07,
			publish: origin.consume(),
		});
		try {
			await violate(pair);
			expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
		} finally {
			connection.abort();
			origin.close();
		}
	});
}

// Reach the Announce Stream `quic` opens: `reset` resets it with `code`, 0 as a browser reports a closing
// session's, and `stop` fails its writes from then on with the error a peer's STOP_SENDING raises.
function announceWriter(quic: MockTransport): { reset(code?: number): void; stop(): void } {
	let announce: WritableStreamDefaultWriter<Uint8Array> | undefined;
	let stopped = false;
	const create = quic.createUnidirectionalStream.bind(quic);
	quic.createUnidirectionalStream = async (options) => {
		const stream = await create(options);
		const getWriter = stream.getWriter.bind(stream);
		stream.getWriter = () => {
			const writer = getWriter();
			const write = writer.write.bind(writer);
			let first = true;
			writer.write = (chunk) => {
				if (first && chunk?.[0] === DataType.Announce) announce = writer;
				first = false;
				if (stopped && writer === announce) {
					return Promise.reject(
						Object.assign(new Error("stopped"), { source: "stream", streamErrorCode: 0 }),
					);
				}
				return write(chunk);
			};
			return writer;
		};
		return stream;
	};
	return {
		reset: (code = 0) =>
			void defined(announce).abort(Object.assign(new Error("gone"), { source: "stream", streamErrorCode: code })),
		stop: () => {
			stopped = true;
		},
	};
}

// A server closing a session resets its Announce Stream and stops the client's before its close
// arrives, with a code Chrome reports as an application's 0: the client takes the server for
// leaving, its routes end, and the session's close says why.
for (const name of ["resets", "stops"] as const) {
	test(`an Announce Stream the peer ${name} ends its routes and leaves the session's close to say why`, async () => {
		const pair = createMockTransportPair(ALPN_07_WIP);
		const serverAnnounce = announceWriter(pair.server);
		const clientAnnounce = announceWriter(pair.client);
		const up = new Producer();
		const down = new Producer();
		const fromClient = new Producer();
		const fromServer = new Producer();
		const url = new URL("https://localhost/test");
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client, publish: up.consume(), consume: fromServer }),
			accept({ url, transport: pair.server, publish: down.consume(), consume: fromClient }),
		]);
		const cam = up.route(Path.from("up/cam"));
		const room = down.route(Path.from("down/room"));
		const atServer = fromClient.routes();
		const atClient = fromServer.routes();
		let closed = false;
		void pair.client.closed.then(() => {
			closed = true;
		});
		const late: { broadcast: { close(): void } }[] = [];
		try {
			expect(await atServer.next()).toMatchObject({ prefix: Path.from("up/cam"), kind: "start" });
			expect(await atClient.next()).toMatchObject({ prefix: Path.from("down/room"), kind: "start" });

			if (name === "resets") {
				serverAnnounce.reset();
			} else {
				clientAnnounce.stop();
				// The client learns of the stop on its next send.
				late.push(up.route(Path.from("up/late")));
			}
			expect(await atClient.next()).toMatchObject({ prefix: Path.from("down/room"), kind: "offline" });
			// What the peer still sends on its own stream about the routes that ended, such as a
			// claim it now withdraws, is ignored.
			if (name === "stops") room.broadcast.close();
			await settle();
			expect(closed).toBe(false);

			pair.server.close({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
			expect(await pair.client.closed).toEqual({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
		} finally {
			atServer.close();
			atClient.close();
			client.abort();
			server.abort();
			cam.broadcast.close();
			room.broadcast.close();
			for (const route of late) route.broadcast.close();
			for (const origin of [up, down, fromClient, fromServer]) origin.close();
		}
	});
}

// A reset carrying an application code of its own is no closing session's: it breaks the session.
test("an Announce Stream the peer resets with a code closes moq-lite-07 with PROTOCOL_VIOLATION", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const serverAnnounce = announceWriter(pair.server);
	const down = new Producer();
	const fromServer = new Producer();
	const url = new URL("https://localhost/test");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume: fromServer }),
		accept({ url, transport: pair.server, publish: down.consume() }),
	]);
	const room = down.route(Path.from("down/room"));
	const atClient = fromServer.routes();
	try {
		expect(await atClient.next()).toMatchObject({ prefix: Path.from("down/room"), kind: "start" });
		serverAnnounce.reset(7);
		expect((await pair.client.closed).closeCode).toBe(SessionCode.ProtocolViolation);
	} finally {
		atClient.close();
		client.abort();
		server.abort();
		room.broadcast.close();
		for (const origin of [down, fromServer]) origin.close();
	}
});

// A peer that stops this side's Announce Stream as it asks for a prefix is leaving too: the reader
// waiting on that ANNOUNCE_REQUEST is not failed with the stop, and the session stays open.
test("an ANNOUNCE_REQUEST the peer stops leaves its reader and the session open", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const clientAnnounce = announceWriter(pair.client);
	const down = new Producer();
	const url = new URL("https://localhost/test");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ url, transport: pair.server, publish: down.consume() }),
	]);
	let closed = false;
	void pair.client.closed.then(() => {
		closed = true;
	});
	clientAnnounce.stop();
	const listing = client.announced(Path.Pattern.literal(ROOM));
	let ended: unknown;
	void listing.closed.then((err) => {
		ended = err ?? "closed";
	});
	try {
		await settle();
		expect(ended).toBeUndefined();
		expect(closed).toBe(false);
	} finally {
		listing.close();
		client.abort();
		server.abort();
		down.close();
	}
});

// The peer finishing its Announce Stream is leaving: its routes end, the session stays, and the
// prefixes it asked for are still answered.
test("a finished Announce Stream ends the peer's routes and keeps moq-lite-07 answering", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const up = new Producer();
	const down = new Producer();
	const fromClient = new Producer();
	const fromServer = new Producer();
	const url = new URL("https://localhost/test");
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, publish: up.consume(), consume: fromServer }),
		accept({ url, transport: pair.server, publish: down.consume(), consume: fromClient }),
	]);
	const cam = up.route(Path.from("up/cam"));
	const room = down.route(Path.from("down/room"));
	const atServer = fromClient.routes();
	const atClient = fromServer.routes();
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});
	try {
		expect(await atServer.next()).toMatchObject({ prefix: Path.from("up/cam"), kind: "start" });
		expect(await atClient.next()).toMatchObject({ prefix: Path.from("down/room"), kind: "start" });

		// The client finishes its stream, as it does when it leaves.
		await AnnounceStream.of(pair.client, version).finish();
		expect(await atServer.next()).toMatchObject({ prefix: Path.from("up/cam"), kind: "offline" });
		expect(closed).toBe(false);

		// What the client asked for is still answered.
		const late = down.route(Path.from("down/late"));
		expect(await atClient.next()).toMatchObject({ prefix: Path.from("down/late"), kind: "start" });
		expect(closed).toBe(false);
		late.broadcast.close();
	} finally {
		atServer.close();
		atClient.close();
		client.abort();
		server.abort();
		cam.broadcast.close();
		room.broadcast.close();
		for (const origin of [up, down, fromClient, fromServer]) origin.close();
	}
});

// Lite-07 has no bidirectional Announce stream: its type is one the version does not know,
// which resets the stream and keeps the session.
test("a bidirectional stream of the Announce type is unknown to moq-lite-07", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const connection = new Connection({ url: new URL("https://relay.example/"), quic: pair.client, version });
	let closed = false;
	void pair.client.closed.then(() => {
		closed = true;
	});
	try {
		const stream = await Stream.open(pair.server, { version });
		await stream.writer.u53(StreamId.Announce);
		await stream.writer.write(new Uint8Array([1, 0]));
		await expect(stream.reader.readAll()).rejects.toThrow();
		await settle();
		expect(closed).toBe(false);
	} finally {
		connection.abort();
	}
});

// Lite-07 removes FETCH (bidi 0x3): it resets only that stream while keeping the session alive
// so subsequent SUBSCRIBE requests still succeed.
test("a bidirectional stream of the Fetch type is unknown to moq-lite-07", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new Producer();
	const route = origin.route(Path.from("room"));
	const producer = defined(route.broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	const g0 = hooks.appendNext(producer);
	g0.writeFrame(textFrame("ok", Timestamp.fromMillis(0)));
	g0.finish();

	const server = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.server,
		version,
		publish: origin.consume(),
	});
	const client = new Connection({
		url: new URL("https://relay.example/"),
		quic: pair.client,
		version,
	});
	let closed = false;
	void pair.server.closed.then(() => {
		closed = true;
	});
	try {
		const stream = await Stream.open(pair.client, { version });
		await stream.writer.u53(StreamId.Fetch);
		await stream.writer.write(new Uint8Array([1, 0]));
		await expect(stream.reader.readAll()).rejects.toThrow();
		await settle();
		expect(closed).toBe(false);

		// Session is still healthy and serves fetches via one-shot SUBSCRIBE.
		const fetched = await wireOf(client).consume(Path.from("room")).track("video").fetchGroup(0n);
		const frame = await fetched.readFrame();
		expect(new TextDecoder().decode(frame?.payload)).toBe("ok");
		fetched.close();
	} finally {
		client.abort();
		server.abort();
		route.broadcast.close();
		origin.close();
	}
});
