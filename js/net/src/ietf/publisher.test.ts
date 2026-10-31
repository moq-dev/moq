import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { Signal } from "@moq/signals";
import type { Credential, Grant } from "../auth.ts";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import * as Epoch from "../epoch.ts";
import { error, ProtocolViolation, SessionCode, SessionError, StreamCode, unauthorized } from "../error.ts";
import { MAX_GROUP_FRAMES } from "../group.ts";
import { type Hop, HopSchema } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair, SessionBroadcast } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Producer as TrackProducer } from "../track.ts";
import { wireOf } from "../wire.ts";
import type * as Cluster from "./cluster.ts";
import { ObjectDatagram } from "./datagram.ts";
import { toRequestCode } from "./error.ts";
import { Fetch, FetchHeader } from "./fetch.ts";
import * as Filter from "./filter.ts";
import { Frame, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { toWire } from "./priority.ts";
import { PublishDone, PublishDoneStatus } from "./publish.ts";
import { PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { NativeSession, type Session } from "./session.ts";
import { GroupOrder, Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { TrackStatusOk, TrackStatusRequest } from "./track.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

function publish(origin: OriginProducer, path: Path.Valid) {
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	return broadcast;
}

const VERSION = Version.DRAFT_18;

/** A session that negotiated MoQ Cluster alone. */
const CLUSTER = { cluster: true, epoch: false };

/** How long to wait for a stream before calling it absent, which is how a regression reports. */
const STREAM_WAIT = 1000;

/** A manual clock, installed until disposed. */
function manual(): Manual & Disposable {
	const clock = new Manual();
	const installed = clock.install();
	return Object.assign(clock, { [Symbol.dispose]: () => installed.close() });
}

/**
 * Whether `read` yields a stream once pending work has armed its timers and {@link STREAM_WAIT}
 * has passed on `clock`, which fires any retry that would open one.
 */
async function opens(read: Promise<{ done: boolean }>, clock: Manual): Promise<boolean> {
	let opened = false;
	read.then(
		(next) => {
			opened = !next.done;
		},
		() => {},
	);
	await clock.advance(Nano.zero);
	await clock.advance(Nano.fromMilli(Milli(STREAM_WAIT)));
	return opened;
}

/** Expect the publisher to open no bidirectional stream within {@link STREAM_WAIT} on `clock`. */
async function noStream(transport: WebTransport, clock: Manual): Promise<void> {
	const reader = transport.incomingBidirectionalStreams.getReader();
	try {
		expect(await opens(reader.read(), clock)).toBe(false);
	} finally {
		reader.releaseLock();
	}
}

/** Long enough for the publish to reach the announce loop's signal. */
const SETTLE = 5;

/**
 * Accept the next stream the publisher opens, or give up rather than hang forever.
 *
 * Reads the queue directly instead of racing {@link Stream.accept}, whose pending read
 * would keep the reader locked after the race resolves and could swallow a later stream.
 */
async function nextStream(transport: WebTransport): Promise<Stream | undefined> {
	const reader =
		transport.incomingBidirectionalStreams.getReader() as ReadableStreamDefaultReader<WebTransportBidirectionalStream>;

	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const next = await Promise.race([
			reader.read(),
			new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), STREAM_WAIT);
			}),
		]);

		if (!next || next.done) return undefined;
		return new Stream({ readable: next.value.readable, writable: next.value.writable, version: VERSION });
	} finally {
		clearTimeout(timer);
		reader.releaseLock();
	}
}

/** Read one PUBLISH_NAMESPACE off a stream the publisher opened. */
async function readPublishNamespace(stream: Stream): Promise<Path.Valid> {
	const typeId = await stream.reader.u53();
	expect(typeId).toBe(PublishNamespace.id);

	const msg = await PublishNamespace.decode(stream.reader, VERSION);
	return msg.trackNamespace;
}

/** Answer a PUBLISH_NAMESPACE, which is what unblocks the announce loop. */
async function acceptPublishNamespace(stream: Stream): Promise<void> {
	await stream.writer.u53(RequestOk.id);
	await new RequestOk().encode(stream.writer, VERSION);
}

/**
 * Decline a PUBLISH_NAMESPACE, which the peer is allowed to do without ending the
 * session. The publisher resets the request as soon as it reads the type, which lands
 * back here as a write error once the refusal is already on the wire.
 *
 * `retryInterval` is what the peer says about coming back, in milliseconds: 0 asks not to
 * be offered the namespace again, and anything else is a minimum wait.
 */
async function declinePublishNamespace(stream: Stream, retryInterval = 1n): Promise<void> {
	try {
		await stream.writer.u53(RequestError.id);
		await new RequestError({
			// UNINTERESTED, draft-19 section 15.11.2.
			errorCode: 0x20,
			reasonPhrase: "no",
			retryInterval,
		}).encode(stream.writer, VERSION);
	} catch {
		// The publisher reset the request out from under us.
	}
}

/**
 * A publisher serving `origin`, which is what a session publishes from: the broadcasts
 * are the origin's, so a test publishes and unpublishes through it rather than the
 * publisher.
 */
function publisher(
	transport: WebTransport,
	{
		requiresSolicitation = false,
		session,
		cluster,
		grant,
		activeCount,
		auth,
		epoch,
	}: {
		requiresSolicitation?: boolean;
		session?: Session;
		cluster?: Cluster.Hops;
		grant?: Signal<Grant | undefined>;
		activeCount?: boolean;
		auth?: Tokens;
		epoch?: boolean;
	} = {},
): { pub: Publisher; origin: OriginProducer } {
	const origin = new OriginProducer();
	const inner = session ?? new NativeSession(transport, VERSION, true);
	return {
		pub: new Publisher({
			quic: transport,
			session: inner,
			publish: origin.consume(),
			requiresSolicitation,
			cluster,
			grant,
			activeCount,
			auth,
			epoch,
		}),
		origin,
	};
}

const IETF_VERSIONS = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;

/** A request stream recording what the publisher writes, with the requester's side under test control. */
function requestStream(version: IetfVersion) {
	let input!: ReadableStreamDefaultController<Uint8Array>;
	const written: number[] = [];
	const stream = new Stream({
		readable: new ReadableStream<Uint8Array>({
			start(c) {
				input = c;
			},
		}),
		writable: new WritableStream<Uint8Array>({
			write: (chunk) => {
				written.push(...chunk);
			},
		}),
		version,
	});
	return {
		stream,
		written,
		wire: () => new Reader(undefined, Uint8Array.from(written), version),
		send: (bytes: Uint8Array) => input.enqueue(bytes),
		cancel: () => input.error(new Error("request cancelled")),
		/**
		 * End the request as `version` cancels one: by FIN through draft 18, and by a reset
		 * after.
		 */
		withdraw: () => (version <= Version.DRAFT_18 ? input.close() : input.error(new Error("request cancelled"))),
	};
}

function trackStatus(namespace: string, propertiesWanted = true) {
	return new TrackStatusRequest({
		requestId: 7n,
		trackNamespace: Path.from(namespace),
		trackName: "video",
		propertiesWanted,
	});
}

/** The error code of the refusal on `wire`. */
async function refusalCode(wire: Reader, version: IetfVersion): Promise<number> {
	expect(await wire.u53()).toBe(RequestError.id);
	return (await RequestError.decode(wire, version)).errorCode;
}

// TRACK_STATUS gets what a SUBSCRIBE_OK would carry, as far as each draft's answer has room,
// without subscribing to the track. INCLUDE_PROPERTIES=0 empties the properties.
test("TRACK_STATUS is answered on every draft without subscribing", async () => {
	for (const version of IETF_VERSIONS) {
		for (const wanted of [true, false]) {
			const pair = createMockTransportPair(ALPNS[version]);
			const session = new NativeSession(pair.server, version, true);
			const { pub, origin } = publisher(pair.server, { session });
			const broadcast = publish(origin, Path.from("room"));
			const track = broadcast.createTrack("video", {
				timescale: Timescale.MILLI,
				maxAge: Milli(5000),
				priority: 2,
			});
			const group = track.createGroup(4);
			group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(0) });
			group.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.fromMillis(1) });
			const subscribed = spyOn(track, "subscribe");

			const { stream, wire } = requestStream(version);
			// Only draft-22 can carry the opt-out.
			await pub.runTrackStatusRequest(trackStatus("room", wanted || version < Version.DRAFT_20), stream);
			expect(subscribed).not.toHaveBeenCalled();

			const r = wire();
			expect(await r.u53()).toBe(TrackStatusOk.id);
			const ok = await TrackStatusOk.decode(r, version);
			expect(await r.done()).toBe(true);

			expect(ok.largest).toEqual({ groupId: 4n, objectId: 1n });
			const expected =
				!wanted && version >= Version.DRAFT_20
					? {}
					: {
							timescale: Timescale.MILLI,
							priority: toWire(2),
							groupOrder: GroupOrder.Descending,
							maxCacheDuration: 5000n,
						};
			expect(ok.properties).toEqual(expected);
			origin.close();
		}
	}
});

// A track that has published nothing answers without LARGEST_OBJECT.
test("TRACK_STATUS for an empty track names no largest", async () => {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPNS[version]);
	const session = new NativeSession(pair.server, version, true);
	const { pub, origin } = publisher(pair.server, { session });
	publish(origin, Path.from("room")).createTrack("video");

	const { stream, wire } = requestStream(version);
	await pub.runTrackStatusRequest(trackStatus("room"), stream);
	const r = wire();
	expect(await r.u53()).toBe(TrackStatusOk.id);
	expect((await TrackStatusOk.decode(r, version)).largest).toBeUndefined();
	origin.close();
});

// Refused before anything resolves when the grant excludes the broadcast, and refused when
// the grant narrows while the broadcast resolves.
test("TRACK_STATUS outside the grant is refused on every draft", async () => {
	const nothing = (): Grant => ({ publish: new Path.Patterns([]), subscribe: new Path.Patterns([]) });
	for (const version of IETF_VERSIONS) {
		const unauthorized = toRequestCode("unauthorized", "track_status", version);
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const grant = new Signal<Grant | undefined>(nothing());
		const { pub, origin } = publisher(pair.server, { session, grant });
		const requests = origin.requestsRoute(Path.from("room")).requested();

		const outside = requestStream(version);
		await pub.runTrackStatusRequest(trackStatus("room"), outside.stream);
		expect(await refusalCode(outside.wire(), version)).toBe(unauthorized);

		// Granted, so it reaches the handler, which has not answered when the grant narrows.
		grant.set(undefined);
		const narrowed = requestStream(version);
		const task = pub.runTrackStatusRequest(trackStatus("room"), narrowed.stream);
		expect((await requests.next()).value?.path).toBe(Path.from("room"));
		grant.set(nothing());
		await task;
		expect(await refusalCode(narrowed.wire(), version)).toBe(unauthorized);

		await requests.return?.();
		origin.close();
	}
});

// Nothing answers a SUBSCRIBE while its broadcast resolves, yet the request stream is still
// read: a second GOAWAY then closes the session at once, not once an answer goes out.
test("a second GOAWAY on an unanswered SUBSCRIBE closes the session", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const { pub, origin } = publisher(pair.server, { session });
		const requests = origin.requests(Path.from("room")).requested();

		const held = requestStream(version);
		const task = pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("room"),
				trackName: "video",
				subscriberPriority: 0,
			}),
			held.stream,
		);
		expect((await requests.next()).value?.path).toBe(Path.from("room"));
		// GOAWAY with no new session and no timeout, twice.
		held.send(Uint8Array.of(0x10, 0, 2, 0, 0, 0x10, 0, 2, 0, 0));

		const closed = await Promise.race([
			pair.server.closed,
			new Promise<never>((_resolve, reject) => setTimeout(() => reject(new Error("session stayed up")), 500)),
		]);
		expect(closed.closeCode).toBe(SessionCode.ProtocolViolation);
		await task;
		expect(held.written).toEqual([]);

		await requests.return?.();
		origin.close();
	}
});

test("TRACK_STATUS for a missing broadcast or track is refused DOES_NOT_EXIST", async () => {
	for (const version of IETF_VERSIONS) {
		const missing = toRequestCode("does_not_exist", "track_status", version);
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const { pub, origin } = publisher(pair.server, { session });

		const absent = requestStream(version);
		await pub.runTrackStatusRequest(trackStatus("absent"), absent.stream);
		expect(await refusalCode(absent.wire(), version)).toBe(missing);

		// A produced broadcast serves only the tracks it created.
		publish(origin, Path.from("room"));
		const untracked = requestStream(version);
		await pub.runTrackStatusRequest(trackStatus("room"), untracked.stream);
		expect(await refusalCode(untracked.wire(), version)).toBe(missing);
		origin.close();
	}
});

// The requester leaving is owed nothing, and gives back the origin demand or the track
// lookup it held, whichever was still resolving.
test("a cancelled TRACK_STATUS releases its demand on every draft", async () => {
	for (const version of IETF_VERSIONS) {
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const { pub, origin } = publisher(pair.server, { session });
		const requests = origin.requestsRoute(Path.from("room")).requested();

		// The broadcast is still resolving: its handler never answers.
		const early = requestStream(version);
		const resolving = pub.runTrackStatusRequest(trackStatus("room/early"), early.stream);
		const pending = (await requests.next()).value;
		expect(pending?.path).toBe(Path.from("room/early"));
		early.cancel();
		await resolving;
		expect(early.written).toEqual([]);
		// Withdrawn, so a late answer is closed rather than served.
		const answer = new SessionBroadcast();
		hooks.acceptRequest(pending, answer);
		expect(answer.closed.peek()).toBeNull();

		// The broadcast resolves, but its track is never answered.
		const late = requestStream(version);
		const looking = pub.runTrackStatusRequest(trackStatus("room/late"), late.stream);
		const request = (await requests.next()).value;
		if (!request) throw new Error("no request for room/late");
		const served = new BroadcastProducer();
		const trackRequest = wireOf(served).requested();
		hooks.acceptRequest(request, served);
		expect((await trackRequest)?.name).toBe("video");
		expect(served.demand().used.peek()).toBe(true);
		late.cancel();
		await looking;
		expect(late.written).toEqual([]);
		await served.demand().unused();

		await requests.return?.();
		served.close();
		origin.close();
	}
});

// The requester leaving while a SUBSCRIBE's setup waits on a producer that never answers is
// owed nothing, and gives back the origin demand or the track it asked for at once.
test("a SUBSCRIBE withdrawn during setup releases its demand on every draft", async () => {
	const subscribe = (namespace: string) =>
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from(namespace),
			trackName: "video",
			subscriberPriority: 0,
		});
	for (const version of IETF_VERSIONS) {
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const { pub, origin } = publisher(pair.server, { session });
		const requests = origin.requestsRoute(Path.from("room")).requested();

		// The broadcast is still resolving: its handler never answers.
		const early = requestStream(version);
		const resolving = pub.runSubscribe(subscribe("room/early"), early.stream);
		const pending = (await requests.next()).value;
		expect(pending?.path).toBe(Path.from("room/early"));
		early.withdraw();
		await resolving;
		expect(early.written).toEqual([]);
		// Withdrawn, so a late answer is closed rather than served.
		const answer = new SessionBroadcast();
		if (pending) hooks.acceptRequest(pending, answer);
		expect(answer.closed.peek()).toBeNull();

		// The broadcast resolves, but its track is never answered.
		const late = requestStream(version);
		const subscribing = pub.runSubscribe(subscribe("room/late"), late.stream);
		const request = (await requests.next()).value;
		if (!request) throw new Error("no request for room/late");
		const served = new BroadcastProducer();
		const trackRequest = wireOf(served).requested();
		hooks.acceptRequest(request, served);
		expect((await trackRequest)?.name).toBe("video");
		expect(served.demand().used.peek()).toBe(true);
		late.withdraw();
		await subscribing;
		expect(late.written).toEqual([]);
		await served.demand().unused();

		await requests.return?.();
		served.close();
		origin.close();
	}
});

// A resume subscribes again as a new SUBSCRIBE does, so the requester leaving while the
// resumed track is never answered ends the request at once too.
test("a SUBSCRIBE withdrawn while resuming releases its demand", async () => {
	for (const version of IETF_VERSIONS) {
		const fx = fixture(version);
		const client = await Stream.open(fx.pair.client, { version });
		const server = await Stream.accept(fx.pair.server, version);
		if (!server) throw new Error("publisher never accepted the subscribe stream");
		const first = (async () => {
			const request = await wireOf(fx.broadcast).requested();
			if (!request) throw new Error("no track was requested");
			return request.accept({ timescale: Timescale.MILLI });
		})();
		const serving = fx.pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 128,
				forward: false,
			}),
			server,
		);
		try {
			expect(await client.reader.u53()).toBe(SubscribeOk.id);
			await SubscribeOk.decode(client.reader, version);
			const track = await first;
			while (track.subscription.peek() !== undefined) await track.subscription.changed();
			// Gone, so the resume asks for the track again, and nothing answers.
			track.finish();
			track.close();
			const second = wireOf(fx.broadcast).requested();
			const parameters = new Parameters();
			parameters.forward = true;
			await client.writer.u53(SubscribeUpdate.id);
			await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, version);
			const resumed = await second;
			if (!resumed) throw new Error("the resume asked for nothing");
			expect(resumed.demand().wanted.peek()).not.toBeUndefined();

			if (version <= Version.DRAFT_18) client.writer.close();
			else client.reader.stop(new Error("cancel"));
			await serving;
			expect(resumed.demand().wanted.peek()).toBeUndefined();
		} finally {
			client.close();
			fx.close();
		}
	}
});

test("Range Filters over MAX_FILTER_RANGES zero get INVALID_FILTER", async () => {
	for (const version of [Version.DRAFT_22]) {
		const pair = createMockTransportPair(ALPNS[version]);
		const { pub, origin } = publisher(pair.server, { session: new NativeSession(pair.server, version, true) });
		for (const fetch of [false, true]) {
			const request = requestStream(version);
			try {
				const name = [1, 4, ...new TextEncoder().encode("test"), 5, ...new TextEncoder().encode("video")];
				const fields = [7, ...name, 1, 0x25, 0];
				const encoded = new Reader(undefined, Uint8Array.from([0, fields.length, ...fields]), version);
				if (fetch) await pub.runFetch(await Fetch.decode(encoded, version), request.stream);
				else await pub.runSubscribe(await Subscribe.decode(encoded, version), request.stream);
				await request.stream.writer.closed;
				const wire = request.wire();
				expect(await wire.u53()).toBe(RequestError.id);
				expect((await RequestError.decode(wire, version)).errorCode).toBe(0x36);
			} finally {
				request.cancel();
			}
		}
		origin.close();
	}
});

test("RENDEZVOUS_TIMEOUT has a zero cap with TIMEOUT for a requested wait", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		// A wait applies only to a missing publisher; a missing track on a live one does not exist.
		for (const [namespace, trackName, live] of [
			["missing", "video", false],
			["room", "audio", true],
		] as const) {
			for (const millis of [undefined, 0, 7]) {
				const pair = createMockTransportPair(ALPNS[version]);
				const { pub, origin } = publisher(pair.server, {
					session: new NativeSession(pair.server, version, true),
				});
				const broadcast = live ? publish(origin, Path.from(namespace)) : undefined;
				const track = broadcast?.createTrack("video", { timescale: Timescale.MILLI });
				const request = requestStream(version);
				const fields = [
					7,
					1,
					namespace.length,
					...new TextEncoder().encode(namespace),
					trackName.length,
					...new TextEncoder().encode(trackName),
					...(millis === undefined ? [0] : [1, 0x04, millis]),
				];
				const msg = await Subscribe.decode(
					new Reader(undefined, Uint8Array.from([0, fields.length, ...fields]), version),
					version,
				);
				try {
					await pub.runSubscribe(msg, request.stream);
					await request.stream.writer.closed;
					const wire = request.wire();
					expect(await wire.u53()).toBe(RequestError.id);
					expect((await RequestError.decode(wire, version)).errorCode).toBe(millis && !live ? 0x02 : 0x10);
				} finally {
					track?.finish();
					track?.close();
					broadcast?.close();
					origin.close();
					request.cancel();
				}
			}
		}
	}
});

test("reserved namespace requests get DOES_NOT_EXIST while other dot namespaces are served", async () => {
	for (const version of [Version.DRAFT_22]) {
		for (const path of [".", "./child", ".session", ".session/extension", ".application", ".sessionish"]) {
			const pair = createMockTransportPair(ALPNS[version]);
			const { pub, origin } = publisher(pair.server, { session: new NativeSession(pair.server, version, true) });
			// Navigation segments cannot be published. A covering application
			// route still proves these reserved requests are refused before lookup.
			const navigation = path.split("/")[0] === ".";
			const claim = navigation ? origin.requests(Path.empty()) : undefined;
			const requested = claim?.requested().next();
			const broadcast = navigation ? undefined : publish(origin, Path.from(path));
			const track = broadcast?.createTrack("video", { timescale: Timescale.MILLI });
			const request = requestStream(version);
			try {
				await pub.runTrackStatusRequest(trackStatus(path), request.stream);
				await request.stream.writer.closed;
				const wire = request.wire();
				if (path.split("/")[0] === "." || path.split("/")[0] === ".session") {
					expect(await wire.u53()).toBe(RequestError.id);
					expect((await RequestError.decode(wire, version)).errorCode).toBe(0x10);
				} else {
					expect(await wire.u53()).toBe(TrackStatusOk.id);
				}
			} finally {
				request.cancel();
				track?.finish();
				track?.close();
				broadcast?.close();
				claim?.close();
				origin.close();
				if (requested) expect((await requested).done).toBe(true);
			}
		}
	}
});

/** A request's USE_VALUE token decodes but grants nothing yet, so a SUBSCRIBE outside the grant is still refused. */
test("a request token does not widen the grant", async () => {
	for (const version of [Version.DRAFT_22] as const) {
		const pair = createMockTransportPair(ALPNS[version]);
		const session = new NativeSession(pair.server, version, true);
		const nothing = new Signal<Grant | undefined>({
			publish: new Path.Patterns([]),
			subscribe: new Path.Patterns([]),
		});
		const { pub, origin } = publisher(pair.server, { session, grant: nothing });
		publish(origin, Path.from("room"));

		const ascii = (text: string) => [...new TextEncoder().encode(text)];
		const token = [0x03, 0x00, 0xaa]; // USE_VALUE, type 0
		const body = [0, 1, 4, ...ascii("room"), 5, ...ascii("video"), 1, 0x03, token.length, ...token];
		const msg = await Subscribe.decode(
			new Reader(undefined, new Uint8Array([0, body.length, ...body]), version),
			version,
		);

		const written: Uint8Array[] = [];
		const stream = new Stream({
			readable: new ReadableStream<Uint8Array>(),
			writable: new WritableStream<Uint8Array>({
				write: (chunk) => {
					written.push(new Uint8Array(chunk));
				},
			}),
			version,
		});
		await pub.runSubscribe(msg, stream);
		await stream.writer.closed;
		origin.close();

		// SUBSCRIBE_ERROR on draft-14, REQUEST_ERROR after; the code follows the Length and any Request ID.
		const wire = written.flatMap((chunk) => Array.from(chunk));
		expect(wire[0]).toBe(0x05);
		expect(wire[3]).toBe(toRequestCode("unauthorized", "subscribe", version));
	}
});

// The header is part of the group's lifetime too. If it blocks on flow control, advancing
// the live edge must reset the stream without waiting for that write to finish.
test("a blocked group header is reset when the group expires", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);

	let started!: () => void;
	const headerStarted = new Promise<void>((resolve) => {
		started = resolve;
	});
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	let reset!: () => void;
	const streamReset = new Promise<void>((resolve) => {
		reset = resolve;
	});
	const closed = new Promise<void>(() => {});
	const writable = {
		getWriter: () => ({
			closed,
			write: async () => {
				started();
				await blocked;
			},
			close: async () => {},
			abort: async () => {
				reset();
			},
		}),
		abort: async () => {},
	} as unknown as WritableStream<Uint8Array>;
	pair.server.createUnidirectionalStream = async () => writable;

	const { pub, origin } = publisher(pair.server);
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const client = await Stream.open(pair.client, { version: VERSION });
	const server = await Stream.accept(pair.server, VERSION);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		void pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
			}),
			server,
		);

		const old = track.createGroup(0);
		old.writeFrame({ payload: new TextEncoder().encode("old"), timestamp: Timestamp.fromMillis(0) });
		old.finish();
		await headerStarted;

		const edge = track.createGroup(1);
		edge.writeFrame({ payload: new TextEncoder().encode("edge"), timestamp: Timestamp.fromMillis(10_000) });
		edge.finish();

		// A group beyond the edge, so group 0's reach (10s, where group 1 begins) is
		// provably past the budget. A group is bounded by where its successor starts, so
		// the successor alone never convicts it: nothing yet proves group 0 ends sooner.
		const later = track.createGroup(2);
		later.writeFrame({ payload: new TextEncoder().encode("later"), timestamp: Timestamp.fromMillis(20_000) });
		later.finish();

		const resetBeforeRelease = await Promise.race([
			streamReset.then(() => true),
			new Promise<false>((resolve) => setTimeout(() => resolve(false), 500)),
		]);
		expect(resetBeforeRelease).toBe(true);
	} finally {
		release();
		client.close();
		broadcast.close();
		origin.close();
	}
});

// A finished group's stream still holds the connection until its FIN is acknowledged. One
// whose FIN never completes is waited on only while the group is within the budget, then
// reset like a stream still serving, as the Rust `GroupServe` does. The track ending first
// does not end the wait: PUBLISH_DONE waits on the stream, so the budget still judges it.
test("a group stream whose FIN never completes is reset when the group expires", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(ALPN.DRAFT_18);

	let closing!: () => void;
	const closeStarted = new Promise<void>((resolve) => {
		closing = resolve;
	});
	let reset = false;
	let resetting!: () => void;
	const streamReset = new Promise<void>((resolve) => {
		resetting = resolve;
	});
	const stalled = new WritableStream<Uint8Array>({
		start(controller) {
			// An abort while the close is in flight reaches the sink only through its signal.
			controller.signal.addEventListener("abort", () => {
				reset = true;
				resetting();
			});
		},
		close() {
			closing();
			return new Promise<void>(() => {});
		},
	});
	spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => stalled);

	const budget = Milli(5000);
	const { pub, origin } = publisher(pair.server);
	const broadcast = publish(origin, Path.from("test"));
	// No subscriber max age, so the track's caps the budget.
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: budget });
	const client = await Stream.open(pair.client, { version: VERSION });
	const server = await Stream.accept(pair.server, VERSION);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		void pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
			}),
			server,
		);

		const write = (sequence: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: new TextEncoder().encode("frame"), timestamp: Timestamp.fromMillis(0) });
			group.finish();
		};

		write(0);
		await closeStarted;
		// The newest group is never stale, however long its FIN takes.
		await clock.advance(Nano.fromMilli(Milli(budget * 2)));
		expect(reset).toBe(false);

		// Its successor presents no further, so only the wall clock since it arrived convicts
		// group 0, after the track has ended.
		write(1);
		track.finish();
		track.close();
		await clock.advance(Nano.fromMilli(Milli(budget - 1)));
		expect(reset).toBe(false);
		await clock.advance(Nano.fromMilli(Milli(1)));
		await streamReset;
	} finally {
		client.close();
		broadcast.close();
		origin.close();
		installed.close();
	}
});

// Under a budget past the cache window, the cache evicts a stalled group and its successor
// before the budget convicts the group. The verdict still waits only for the successor's
// arrival plus the budget, not for whichever newer group the cache holds, and PUBLISH_DONE
// follows the reset.
test("a group stream whose FIN never completes is reset after the cache evicts it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(ALPN.DRAFT_18);

	let closing!: () => void;
	const closeStarted = new Promise<void>((resolve) => {
		closing = resolve;
	});
	let reset = false;
	let resetting!: () => void;
	const streamReset = new Promise<void>((resolve) => {
		resetting = resolve;
	});
	const stalled = new WritableStream<Uint8Array>({
		start(controller) {
			controller.signal.addEventListener("abort", () => {
				reset = true;
				resetting();
			});
		},
		close() {
			closing();
			return new Promise<void>(() => {});
		},
	});
	spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => stalled);

	// Twice the cache's idle window.
	const budget = Milli(60_000);
	const { pub, origin } = publisher(pair.server);
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: budget });
	const client = await Stream.open(pair.client, { version: VERSION });
	const server = await Stream.accept(pair.server, VERSION);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		const running = pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
			}),
			server,
		);
		expect(await client.reader.u53()).toBe(SubscribeOk.id);
		await SubscribeOk.decode(client.reader, VERSION);

		// Frozen timestamps, so only the wall clock since group 1 arrived convicts group 0.
		const write = (sequence: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: new TextEncoder().encode("frame"), timestamp: Timestamp.fromMillis(0) });
			group.finish();
		};

		write(0);
		await closeStarted;
		write(1);
		// A later group, so a verdict that forgot group 1 would wait for this one's deadline.
		const later = Milli(10_000);
		await clock.advance(Nano.fromMilli(later));
		write(2);
		track.finish();
		track.close();

		await clock.advance(Nano.fromMilli(Milli(budget - later - 1)));
		expect(reset).toBe(false);
		await clock.advance(Nano.fromMilli(Milli(1)));
		await streamReset;

		await running;
		expect(await client.reader.u53()).toBe(PublishDone.id);
		await PublishDone.decode(client.reader, VERSION);
	} finally {
		client.close();
		broadcast.close();
		origin.close();
		installed.close();
	}
});

// A group can go stale while its stream is still opening. Serving it must abandon the group
// without starting a write: an abandoned write rejects once the stream resets, and nothing
// would handle it (Node exits on the first unhandled rejection).
test("a group that goes stale while its stream opens writes nothing", async () => {
	const unhandled: unknown[] = [];
	const onUnhandled = (reason: unknown) => unhandled.push(reason);

	const pair = createMockTransportPair(ALPN.DRAFT_18);
	let requested!: () => void;
	const opening = new Promise<void>((resolve) => {
		requested = resolve;
	});
	let open!: () => void;
	const opened = new Promise<void>((resolve) => {
		open = resolve;
	});
	let reset!: (reason: unknown) => void;
	const streamReset = new Promise<unknown>((resolve) => {
		reset = resolve;
	});
	let writes = 0;
	const stale = new WritableStream<Uint8Array>({
		write() {
			writes++;
			throw new Error("write into an abandoned stream");
		},
		abort: (reason) => reset(reason),
	});
	spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => {
		requested();
		await opened;
		return stale;
	});

	const { pub, origin } = publisher(pair.server);
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const client = await Stream.open(pair.client, { version: VERSION });
	const server = await Stream.accept(pair.server, VERSION);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		process.on("unhandledRejection", onUnhandled);
		void pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
			}),
			server,
		);

		const write = (sequence: number, ms: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: new TextEncoder().encode("frame"), timestamp: Timestamp.fromMillis(ms) });
			group.finish();
		};
		write(0, 0);
		await opening;

		// A group beyond the edge, so group 0's reach (where group 1 begins) is provably past
		// the budget: a successor alone never convicts it.
		write(1, 10_000);
		write(2, 20_000);
		open();

		expect(String(await streamReset)).toContain("abandoned waiting for a stream slot");
		expect(writes).toBe(0);
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(unhandled).toEqual([]);
	} finally {
		process.off("unhandledRejection", onUnhandled);
		client.close();
		broadcast.close();
		origin.close();
	}
});

/**
 * Every advertisement waits a round trip for the peer's reply. A broadcast published in
 * that window has to survive it: the loop is not watching the signal while it waits, so
 * a listener registered afterwards would sleep through the notification and leave the
 * namespace unadvertised until something unrelated changed.
 */
test("a broadcast published mid-advertisement is still announced", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	publish(origin, Path.from("first"));

	void pub.runPublishNamespaces();

	// Take the first advertisement but withhold the reply, parking the loop.
	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));

	// Publish while the loop is parked on that reply.
	publish(origin, Path.from("second"));
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	await acceptPublishNamespace(one);

	const two = await nextStream(pair.client);
	if (!two) throw new Error("the broadcast published mid-advertisement was never announced");
	expect(await readPublishNamespace(two)).toBe(Path.from("second"));
	await acceptPublishNamespace(two);

	origin.close();
});

/**
 * A peer may decline an advertisement and stay connected. Recording it as advertised
 * anyway would strand the namespace: nothing re-adds it to the diff, so it would never
 * be offered again for the life of the session.
 */
test("a declined advertisement is retried on the next change", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	publish(origin, Path.from("first"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(declined)).toBe(Path.from("first"));
	await declinePublishNamespace(declined);

	// Any later change re-runs the diff, which is where the refused namespace has to
	// reappear rather than being remembered as up.
	publish(origin, Path.from("second"));

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("second"));
	expect(seen).toContain(Path.from("first"));

	origin.close();
});

/**
 * A SUBSCRIBE_NAMESPACE below an advertised route still hears that route: it serves the
 * requested prefix, so it lands as the empty suffix, then paths beneath the prefix follow
 * as their own suffixes. Matches the Lite publisher and Rust.
 */
test("a subscription below an advertised route hears it as the empty suffix", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { requiresSolicitation: true });
	publish(origin, Path.from("dash"));

	const subscription = await Stream.open(pair.client, { version: VERSION });
	const accepted = await Stream.accept(pair.server, VERSION);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(
		new SubscribeNamespace({ requestId: 0n, namespace: Path.from("dash/nobody") }),
		accepted,
	);

	const entry = async () => {
		expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
		return (await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix;
	};

	expect(await subscription.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(subscription.reader, VERSION);
	expect(await entry()).toBe(Path.empty());

	publish(origin, Path.from("dash/nobody/cam"));
	expect(await entry()).toBe(Path.from("cam"));

	subscription.close();
	origin.close();
});

/**
 * A scoped route covers only what it claims: `room` claiming `room/chat` cannot serve
 * `room/video`, so a subscription there must not hear it as the empty suffix.
 */
test("a subscription outside a covering route's claim does not hear it", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { requiresSolicitation: true });
	const chat = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("room/chat"))]));
	const dynamic = chat.requestsRoute(Path.from("room"));

	const subscription = await Stream.open(pair.client, { version: VERSION });
	const accepted = await Stream.accept(pair.server, VERSION);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(
		new SubscribeNamespace({ requestId: 0n, namespace: Path.from("room/video") }),
		accepted,
	);

	expect(await subscription.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(subscription.reader, VERSION);

	// The first entry is the broadcast beneath the prefix, not the out-of-claim cover.
	publish(origin, Path.from("room/video/cam"));
	expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
	expect((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix).toBe(Path.from("cam"));

	dynamic.close();
	subscription.close();
	origin.close();
});

/**
 * A cheaper route claiming `room/chat` does not hide a costlier `room/video` route at the
 * same prefix from a subscription at `room/video`.
 */
test("a subscription hears the best covering route its claim allows", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { requiresSolicitation: true });
	const scoped = (path: string) =>
		origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from(path))]));
	const chat = scoped("room/chat").requestsRoute(Path.from("room"), { cost: 1n });
	const video = scoped("room/video").requestsRoute(Path.from("room"), { cost: 5n });

	const subscription = await Stream.open(pair.client, { version: VERSION });
	const accepted = await Stream.accept(pair.server, VERSION);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(
		new SubscribeNamespace({ requestId: 0n, namespace: Path.from("room/video") }),
		accepted,
	);

	expect(await subscription.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(subscription.reader, VERSION);

	// The first entry is the video route as the empty suffix, not a later broadcast beneath it.
	publish(origin, Path.from("room/video/cam"));
	expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
	expect((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix).toBe(Path.empty());

	video.close();
	chat.close();
	subscription.close();
	origin.close();
});

/**
 * A served root collapses every covering route to the empty suffix. A narrow one claiming
 * only `tenant/chat` must not hide a broader one from a subscription at `video`.
 */
test("a subscription hears a broader covering route when the narrowest cannot serve it", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const origin = new OriginProducer();
	const tenant = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.all()]));
	const pub = new Publisher({
		quic: pair.server,
		session: new NativeSession(pair.server, VERSION, true),
		publish: tenant.consume(),
		requiresSolicitation: true,
	});
	const chat = origin
		.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("tenant/chat"))]))
		.requestsRoute(Path.from("tenant"));
	const broad = origin.requestsRoute(Path.empty(), { cost: 5n });

	const subscription = await Stream.open(pair.client, { version: VERSION });
	const accepted = await Stream.accept(pair.server, VERSION);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.from("video") }), accepted);

	expect(await subscription.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(subscription.reader, VERSION);

	// The first entry is the broad route as the empty suffix, not a later broadcast beneath it.
	publish(origin, Path.from("tenant/video/cam"));
	expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
	expect((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix).toBe(Path.empty());

	broad.close();
	chat.close();
	subscription.close();
	origin.close();
});

/**
 * The claim is presented relative to the served origin's root, like the route's key: a
 * publisher serving `tenant` offers `room` (claiming `tenant/room/chat`) to a
 * subscription at `room/chat`.
 */
test("a covering route's claim is compared relative to the served root", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const origin = new OriginProducer();
	const tenant = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.all()]));
	const pub = new Publisher({
		quic: pair.server,
		session: new NativeSession(pair.server, VERSION, true),
		publish: tenant.consume(),
		requiresSolicitation: true,
	});
	const chat = origin.scope(Path.empty(), new Path.Patterns([Path.Pattern.subtree(Path.from("tenant/room/chat"))]));
	const dynamic = chat.requestsRoute(Path.from("tenant/room"));

	const subscription = await Stream.open(pair.client, { version: VERSION });
	const accepted = await Stream.accept(pair.server, VERSION);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(
		new SubscribeNamespace({ requestId: 0n, namespace: Path.from("room/chat") }),
		accepted,
	);

	expect(await subscription.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(subscription.reader, VERSION);
	expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
	expect((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix).toBe(Path.empty());

	dynamic.close();
	subscription.close();
	origin.close();
});

/**
 * A peer that refuses an advertisement with a retry interval of 0 is asking not to be
 * offered it again. Coming back anyway turns a permanent refusal (unauthorized,
 * uninterested) into a request every few seconds for the life of the session.
 */
test("a refusal that forbids retrying is not retried", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);
	publish(origin, Path.from("lonely"));

	void pub.runPublishNamespaces();

	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await declinePublishNamespace(stream, 0n);

	// Well past the retry the loop would otherwise take.
	await noStream(pair.client, clock);

	origin.close();
});

/**
 * The same rule with no gap to observe: republishing a path swaps the routing front in one
 * mutation, so the path never leaves the origin's map and only the front says the peer is
 * being offered a different broadcast. Keying the refusal on the path alone would strand
 * the replacement for the life of the session.
 */
test("republishing a path clears a refusal without unannouncing first", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	publish(origin, Path.from("recycled"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("recycled"));
	await declinePublishNamespace(declined, 0n);
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	// A new broadcast takes the path over outright: no close, no gap.
	publish(origin, Path.from("recycled"));

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("the replacement broadcast was never offered");
	expect(await readPublishNamespace(retried)).toBe(Path.from("recycled"));
	await acceptPublishNamespace(retried);

	origin.close();
});

/**
 * A new epoch on the same broadcast handle is another publisher instance, so a refusal of
 * the old one does not strand it. Rust gets this from the END and START an epoch change
 * delivers.
 */
test("a new epoch at the same path clears a refusal", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	const broadcast = origin.createBroadcast(Path.from("restarted"));
	broadcast.announce({ epoch: Epoch.mint() });

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("restarted"));
	await declinePublishNamespace(declined, 0n);
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	broadcast.announce({ ...broadcast.route, epoch: Epoch.mint() });

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("the new instance was never offered");
	expect(await readPublishNamespace(retried)).toBe(Path.from("restarted"));
	await acceptPublishNamespace(retried);

	origin.close();
});

/**
 * A refusal belongs to the namespace, not the path forever. Unannouncing takes it with
 * it, so a fresh broadcast at the same path is offered again; keeping it would strand
 * that path for the life of the session with no timer able to recover it. Rust gets this
 * by rebuilding the watched entry on re-announce.
 */
test("re-announcing a path clears a refusal that forbade retrying", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	const first = publish(origin, Path.from("recycled"));

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("recycled"));
	await declinePublishNamespace(declined, 0n);

	// The broadcast goes away, taking the refusal with it, and a new one takes its place.
	first.close();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));
	publish(origin, Path.from("recycled"));

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("a re-announced path was never offered again");
	expect(await readPublishNamespace(retried)).toBe(Path.from("recycled"));
	await acceptPublishNamespace(retried);

	origin.close();
});

/**
 * The origin outlives the session and its signal never ends, so a closed connection
 * reaches this loop through nothing it watches. Left unbounded it parks on the shared
 * origin forever, waking on an unrelated publish to fail against a dead transport.
 */
test("closing the session ends the unsolicited announce loop", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);

	publish(origin, Path.from("first"));

	const loop = pub.runPublishNamespaces();

	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));
	await acceptPublishNamespace(one);

	// The session ends. The origin is untouched: it is shared, and other sessions keep using it.
	pair.server.close();

	// Ending with the session's error is ending too: the close fails its open streams.
	await Promise.race([
		loop.catch(() => undefined),
		new Promise((_resolve, reject) =>
			setTimeout(() => reject(new Error("the announce loop outlived its session")), STREAM_WAIT),
		),
	]);

	// The origin really did survive the session, so publishing into it is still valid.
	publish(origin, Path.from("second"));
	origin.close();
});

/**
 * A peer that declared a Hop ID gets one on every advertisement: it is what lets the peer
 * tell that an advertisement it hears back came from us. A peer that declared nothing has
 * not read ours either, so sending it the parameters would be a protocol violation.
 */
test("an advertisement carries our hop id once the peer declared one", async () => {
	const self: Hop = HopSchema.parse(7n);

	for (const peer of [HopSchema.parse(9n), undefined]) {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const { pub, origin } = publisher(pair.server, { cluster: { self, peer } });
		publish(origin, Path.from("mine"));
		void pub.runPublishNamespaces();

		const stream = await nextStream(pair.client);
		if (!stream) throw new Error("no PUBLISH_NAMESPACE for the broadcast");
		expect(await stream.reader.u53()).toBe(PublishNamespace.id);

		if (peer === undefined) {
			// Nothing negotiated, so the parameters are absent: reading the message as a
			// negotiated one finds no HOP_PATH and rejects.
			await expect(PublishNamespace.decode(stream.reader, VERSION, CLUSTER)).rejects.toThrow();
		} else {
			// Our own Hop ID is the last entry, and we originate everything we advertise,
			// so it is the only one. The cost is 0: we are already producing the content.
			const msg = await PublishNamespace.decode(stream.reader, VERSION, CLUSTER);
			expect(msg.trackNamespace).toBe(Path.from("mine"));
			expect(msg.params.cluster).toEqual({ hops: [self], cost: 0n });
			await acceptPublishNamespace(stream);
		}

		origin.close();
	}
});

/** A session that negotiated MoQ Epoch alone. */
const EPOCH = { cluster: false, epoch: true };

/**
 * An advertisement names the publisher instance once the peer declared MoQ Epoch. A peer
 * that did not has not read the parameter, so sending it would be a protocol violation.
 */
test("an advertisement carries its epoch only once negotiated", async () => {
	for (const negotiated of [true, false]) {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const { pub, origin } = publisher(pair.server, { epoch: negotiated });
		const epoch = Epoch.mint();
		origin.createBroadcast(Path.from("mine")).announce({ epoch });
		void pub.runPublishNamespaces();

		const stream = await nextStream(pair.client);
		if (!stream) throw new Error("no PUBLISH_NAMESPACE for the broadcast");
		expect(await stream.reader.u53()).toBe(PublishNamespace.id);
		const msg = await PublishNamespace.decode(stream.reader, VERSION, EPOCH);
		expect(msg.params.epoch).toBe(negotiated ? epoch : undefined);
		await acceptPublishNamespace(stream);

		origin.close();
	}
});

/**
 * A new instance at a path is other content, but the namespace never goes absent: the new
 * epoch is advertised first, and the old one withdrawn only once the peer accepted it,
 * since the two streams are unordered. A peer without MoQ Epoch cannot tell the two apart,
 * so it still sees the old one withdrawn first.
 */
test.each([true, false])(
	"an epoch change advertises the new epoch before withdrawing the old (negotiated: %p)",
	async (negotiated) => {
		using clock = manual();
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const { pub, origin } = publisher(pair.server, { epoch: negotiated });
		const streams = accepted(pair.client);
		try {
			const broadcast = origin.createBroadcast(Path.from("mine"));
			const first = Epoch.mint();
			broadcast.announce({ epoch: first });
			void pub.runPublishNamespaces();

			const old = await take(clock, streams);
			expect(await old.reader.u53()).toBe(PublishNamespace.id);
			expect((await PublishNamespace.decode(old.reader, VERSION, EPOCH)).params.epoch).toBe(
				negotiated ? first : undefined,
			);
			await acceptPublishNamespace(old);

			const second = Epoch.mint();
			broadcast.announce({ ...broadcast.route, epoch: second });
			const withdrawn = watch(old.reader.done());

			const next = await take(clock, streams);
			expect(await next.reader.u53()).toBe(PublishNamespace.id);
			expect((await PublishNamespace.decode(next.reader, VERSION, EPOCH)).params.epoch).toBe(
				negotiated ? second : undefined,
			);
			await clock.advance(Nano.zero);
			expect(withdrawn.state).toBe(negotiated ? "pending" : "rejected");

			await acceptPublishNamespace(next);
			expect(await old.reader.done().catch(error)).toMatchObject({ code: StreamCode.Cancel });
		} finally {
			origin.close();
		}
	},
);

/** On a SUBSCRIBE_NAMESPACE stream, another epoch is one NAMESPACE and no NAMESPACE_DONE. */
test("a solicited namespace takes another epoch as one NAMESPACE and no NAMESPACE_DONE", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { requiresSolicitation: true, epoch: true });
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		const first = Epoch.mint();
		broadcast.announce({ epoch: first });

		const subscription = await Stream.open(pair.client, { version: VERSION });
		const stream = await Stream.accept(pair.server, VERSION);
		if (!stream) throw new Error("the subscription stream was never accepted");
		void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), stream);
		const entry = async () => {
			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			return await SubscribeNamespaceEntry.decode(subscription.reader, VERSION, EPOCH);
		};

		expect(await subscription.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(subscription.reader, VERSION);
		expect(await entry()).toMatchObject({ suffix: Path.from("mine"), params: { epoch: first } });

		const second = Epoch.mint();
		broadcast.announce({ ...broadcast.route, epoch: second });
		expect(await entry()).toMatchObject({ suffix: Path.from("mine"), params: { epoch: second } });

		const more = watch(subscription.reader.u53());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		subscription.close();
	} finally {
		origin.close();
	}
});

/** A request that names an instance is served by that one alone, or refused as gone. */
test("a request pinned to another epoch is refused DOES_NOT_EXIST", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const { pub, origin } = publisher(pair.server, { session, epoch: true });
	const epoch = Epoch.mint();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce({ epoch });
	broadcast.createTrack("video");
	origin.createBroadcast(Path.from("epochless")).announce();

	const status = async (namespace: string, pinned: Epoch.Valid | undefined) => {
		const { stream, wire } = requestStream(VERSION);
		const request = new TrackStatusRequest({
			requestId: 7n,
			trackNamespace: Path.from(namespace),
			trackName: "video",
			epoch: pinned,
		});
		await pub.runTrackStatusRequest(request, stream);
		return wire();
	};

	// Control: the named instance, or none at all, is served.
	expect(await (await status("room", epoch)).u53()).toBe(TrackStatusOk.id);
	expect(await (await status("room", undefined)).u53()).toBe(TrackStatusOk.id);

	const missing = toRequestCode("does_not_exist", "track_status", VERSION);
	expect(await refusalCode(await status("room", Epoch.mint()), VERSION)).toBe(missing);
	expect(await refusalCode(await status("epochless", epoch), VERSION)).toBe(missing);

	// An instance never comes back, so asking to wait for a gone one changes nothing.
	for (const rendezvous of [false, true]) {
		for (const [namespace, pinned] of [
			["room", Epoch.mint()],
			["epochless", epoch],
		] as const) {
			const subscribe = requestStream(VERSION);
			await pub.runSubscribe(
				new Subscribe({
					requestId: 7n,
					trackNamespace: Path.from(namespace),
					trackName: "video",
					subscriberPriority: 0,
					rendezvous,
					epoch: pinned,
				}),
				subscribe.stream,
			);
			const r = subscribe.wire();
			expect(await r.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(r, VERSION)).errorCode).toBe(
				toRequestCode("does_not_exist", "subscribe", VERSION),
			);
		}
	}

	origin.close();
});

/** Only a session that negotiated MoQ Epoch may name an instance. */
test("EPOCH on a request without MoQ Epoch is a protocol violation", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server);
	const epoch = Epoch.mint();
	const trackNamespace = Path.from("room");

	await expect(
		pub.runSubscribe(
			new Subscribe({ requestId: 7n, trackNamespace, trackName: "video", subscriberPriority: 0, epoch }),
			requestStream(VERSION).stream,
		),
	).rejects.toThrow(ProtocolViolation);
	await expect(
		pub.runTrackStatusRequest(
			new TrackStatusRequest({ requestId: 7n, trackNamespace, trackName: "video", epoch }),
			requestStream(VERSION).stream,
		),
	).rejects.toThrow(ProtocolViolation);
	await expect(
		pub.runFetch(
			new Fetch({
				requestId: 7n,
				type: {
					kind: "standalone",
					trackNamespace,
					trackName: "video",
					start: { group: 0n, object: 0n },
					end: { group: 0n, object: 0n },
				},
				epoch,
			}),
			requestStream(VERSION).stream,
		),
	).rejects.toThrow(ProtocolViolation);

	origin.close();
});

/** Advance time in steps until `done` holds, failing once `limit` has passed without it. */
async function advanceUntil(clock: Manual, done: () => boolean, limit: number, step = 50) {
	for (let elapsed = 0; !done(); elapsed += step) {
		if (elapsed >= limit) throw new Error(`still waiting after ${limit}ms`);
		await clock.advance(Nano.fromMilli(Milli(step)));
	}
}

/** How a promise has settled so far, read without awaiting it. */
function watch<T>(promise: Promise<T>): { state: "pending" | "resolved" | "rejected"; value?: T } {
	const status: { state: "pending" | "resolved" | "rejected"; value?: T } = { state: "pending" };
	promise.then(
		(value) => {
			status.state = "resolved";
			status.value = value;
		},
		() => {
			status.state = "rejected";
		},
	);
	return status;
}

/**
 * Every stream the publisher opens, in order, collected as they arrive so a test can ask
 * whether one came without a timer.
 */
function accepted(transport: WebTransport): Stream[] {
	const queue: Stream[] = [];
	void (async () => {
		const reader = transport.incomingBidirectionalStreams.getReader();
		for (;;) {
			const next = await reader.read().catch(() => undefined);
			if (!next || next.done) return;
			queue.push(new Stream({ readable: next.value.readable, writable: next.value.writable, version: VERSION }));
		}
	})();
	return queue;
}

/** Take the next stream the publisher opened, failing if it has not opened one. */
async function take(clock: Manual, queue: Stream[]): Promise<Stream> {
	await clock.advance(Nano.zero);
	const stream = queue.shift();
	if (!stream) throw new Error("expected the publisher to open a stream");
	return stream;
}

/** Read one PUBLISH_NAMESPACE and its cluster parameters, then accept it. */
async function acceptClustered(stream: Stream): Promise<Cluster.Advert | undefined> {
	expect(await stream.reader.u53()).toBe(PublishNamespace.id);
	const msg = await PublishNamespace.decode(stream.reader, VERSION, CLUSTER);
	await acceptPublishNamespace(stream);
	return msg.params.cluster;
}

/** Read one REQUEST_UPDATE off a PUBLISH_NAMESPACE stream. */
async function readUpdate(stream: Stream): Promise<Cluster.Update> {
	expect(await stream.reader.u53()).toBe(PublishNamespaceUpdate.id);
	return (await PublishNamespaceUpdate.decode(stream.reader, VERSION)).update;
}

/** A clustered publisher on a manual clock, with every stream it opens collected. */
function clustered(requiresSolicitation = false, auth?: Tokens) {
	const clock = new Manual();
	const installed = clock.install();
	const self: Hop = HopSchema.parse(7n);
	const peer: Hop = HopSchema.parse(9n);
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { cluster: { self, peer }, requiresSolicitation, auth });
	const streams = accepted(pair.client);
	const close = () => {
		origin.close();
		installed.close();
	};
	return { clock, self, pair, pub, origin, streams, close };
}

const VIA: Hop = HopSchema.parse(3n);
const MID: Hop = HopSchema.parse(4n);
const OTHER: Hop = HopSchema.parse(5n);

/**
 * A reprice is a REQUEST_UPDATE on the request that already carries the namespace, with
 * only what changed. A cost that drops to 0 has to say so: REQUEST_UPDATE keeps an omitted
 * parameter, so leaving it out would leave the peer holding the old price.
 */
test("a price change on a held namespace is one REQUEST_UPDATE, not a withdrawal", async () => {
	const { clock, self, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		expect(await acceptClustered(stream)).toEqual({ hops: [VIA, self], cost: 4n });

		broadcast.announce({ hops: [VIA], cost: 8n });
		expect(await readUpdate(stream)).toEqual({ hops: undefined, cost: 8n });
		await acceptPublishNamespace(stream);

		broadcast.announce({ hops: [VIA], cost: 0n });
		expect(await readUpdate(stream)).toEqual({ hops: undefined, cost: 0n });
		await acceptPublishNamespace(stream);

		// Still the one request, still open: nothing was withdrawn or advertised again.
		const more = watch(stream.reader.done());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		expect(streams).toHaveLength(0);
	} finally {
		close();
	}
});

test("a hop path change behind the same publisher is one REQUEST_UPDATE carrying HOP_PATH", async () => {
	const { clock, self, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [VIA, MID], cost: 4n });
		expect(await readUpdate(stream)).toEqual({ hops: [VIA, MID, self], cost: undefined });
		await acceptPublishNamespace(stream);

		const more = watch(stream.reader.done());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		expect(streams).toHaveLength(0);
	} finally {
		close();
	}
});

/**
 * NAMESPACE has no REQUEST_UPDATE; the receiver reads a repeat as a replacement. That holds
 * for a new original publisher too.
 */
test.each([
	["a hop path and cost change", [VIA, MID], 8n],
	["a first-hop change", [OTHER], 4n],
] as const)("a solicited namespace takes %s as one NAMESPACE and no NAMESPACE_DONE", async (_, hops, cost) => {
	const { clock, self, pair, pub, origin, close } = clustered(true);
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });

		const subscription = await Stream.open(pair.client, { version: VERSION });
		const stream = await Stream.accept(pair.server, VERSION);
		if (!stream) throw new Error("the subscription stream was never accepted");
		void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), stream);

		const entry = async () => {
			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			return await SubscribeNamespaceEntry.decode(subscription.reader, VERSION, CLUSTER);
		};

		expect(await subscription.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(subscription.reader, VERSION);
		expect(await entry()).toMatchObject({
			suffix: Path.from("mine"),
			params: { cluster: { hops: [VIA, self], cost: 4n } },
		});

		broadcast.announce({ hops: [...hops], cost });
		expect(await entry()).toMatchObject({
			suffix: Path.from("mine"),
			params: { cluster: { hops: [...hops, self], cost } },
		});

		const more = watch(subscription.reader.u53());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		subscription.close();
	} finally {
		close();
	}
});

/**
 * A different original publisher updates the PUBLISH_NAMESPACE in place, as any other
 * change does: withdrawing it would make the namespace briefly vanish downstream.
 */
test("a first-hop change is one REQUEST_UPDATE carrying HOP_PATH, not a withdrawal", async () => {
	const { clock, self, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [OTHER], cost: 4n });
		expect(await readUpdate(stream)).toEqual({ hops: [OTHER, self], cost: undefined });
		await acceptPublishNamespace(stream);

		const more = watch(stream.reader.done());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		expect(streams).toHaveLength(0);
	} finally {
		close();
	}
});

/** A different broadcast is not an update: it withdraws the old one and advertises again. */
function republish(origin: OriginProducer, old: BroadcastProducer) {
	const next = origin.createBroadcast(Path.from("mine"));
	next.announce({ hops: [VIA], cost: 4n });
	old.close();
}

/** The tokens one session issued, each carried as its own bytes. */
class Tokens {
	readonly ended = new Signal(0);
	#issued = new Map<Credential, Uint8Array>();

	issue(value: string): Credential {
		const token: Credential = {
			grant: new Signal<Grant | undefined>(undefined),
			closed: new Promise(() => {}),
			close: () => {
				this.#issued.delete(token);
				this.ended.update((n) => n + 1);
			},
		};
		this.#issued.set(token, new TextEncoder().encode(value));
		return token;
	}

	carried(token: Credential | undefined): Uint8Array | undefined {
		return token && this.#issued.get(token);
	}
}

/** Record the token each PUBLISH_NAMESPACE and REQUEST_UPDATE carries, as text. */
function carriedTokens() {
	const sent: (string | undefined)[] = [];
	const text = (token?: Uint8Array) => (token ? new TextDecoder().decode(token) : undefined);
	const advertise = PublishNamespace.prototype.encode;
	const update = PublishNamespaceUpdate.prototype.encode;
	const spies = [
		spyOn(PublishNamespace.prototype, "encode").mockImplementation(function (this: PublishNamespace, w, v) {
			sent.push(text(this.token));
			return advertise.call(this, w, v);
		}),
		spyOn(PublishNamespaceUpdate.prototype, "encode").mockImplementation(function (
			this: PublishNamespaceUpdate,
			w,
			v,
		) {
			sent.push(text(this.token));
			return update.call(this, w, v);
		}),
	];
	return {
		sent,
		restore: () => {
			for (const spy of spies) spy.mockRestore();
		},
	};
}

/**
 * A route's token rides its PUBLISH_NAMESPACE, and swapping it renews the request in
 * place: one REQUEST_UPDATE carrying only the new token. Mirrors the Rust publisher.
 */
test("swapping a route's token is one REQUEST_UPDATE carrying it", async () => {
	const tokens = new Tokens();
	const { clock, pub, origin, streams, close } = clustered(false, tokens);
	const { sent, restore } = carriedTokens();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n, auth: tokens.issue("first") });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);
		expect(sent).toEqual(["first"]);

		broadcast.announce({ hops: [VIA], cost: 4n, auth: tokens.issue("second") });
		expect(await readUpdate(stream)).toEqual({ hops: undefined, cost: undefined });
		await acceptPublishNamespace(stream);
		expect(sent).toEqual(["first", "second"]);
		expect(streams).toHaveLength(0);
	} finally {
		restore();
		close();
	}
});

/** A token another session issued is never carried, so swapping between two sends nothing. */
test("a foreign route token is not carried", async () => {
	const foreign = new Tokens();
	const { clock, pub, origin, streams, close } = clustered(false, new Tokens());
	const { sent, restore } = carriedTokens();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n, auth: foreign.issue("first") });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);
		broadcast.announce({ hops: [VIA], cost: 4n, auth: foreign.issue("second") });
		const more = watch(stream.reader.u53());
		await clock.advance(Nano.zero);
		expect(more.state).toBe("pending");
		expect(sent).toEqual([undefined]);
	} finally {
		restore();
		close();
	}
});

/** REQUEST_UPDATE cannot name a token on a request that had none, so the request restarts. */
test("naming a token on an unnamed request makes it again", async () => {
	const tokens = new Tokens();
	const { clock, pub, origin, streams, close } = clustered(false, tokens);
	const { sent, restore } = carriedTokens();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const old = await take(clock, streams);
		await acceptClustered(old);
		broadcast.announce({ hops: [VIA], cost: 4n, auth: tokens.issue("first") });

		expect(await old.reader.done().catch(error)).toMatchObject({ code: StreamCode.Cancel });
		await acceptClustered(await take(clock, streams));
		expect(sent).toEqual([undefined, "first"]);
	} finally {
		restore();
		close();
	}
});

/** Closing a route's token drops it from the request, which REQUEST_UPDATE cannot do. */
test("closing a route's token makes its request again without it", async () => {
	const tokens = new Tokens();
	const { clock, pub, origin, streams, close } = clustered(false, tokens);
	const { sent, restore } = carriedTokens();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		const token = tokens.issue("first");
		broadcast.announce({ hops: [VIA], cost: 4n, auth: token });
		void pub.runPublishNamespaces();

		const old = await take(clock, streams);
		await acceptClustered(old);
		token.close();

		expect(await old.reader.done().catch(error)).toMatchObject({ code: StreamCode.Cancel });
		await acceptClustered(await take(clock, streams));
		expect(sent).toEqual(["first", undefined]);
	} finally {
		restore();
		close();
	}
});

test("a republish withdraws the PUBLISH_NAMESPACE and advertises again", async () => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		const broadcastPublication = origin.createBroadcast(Path.from("mine"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const old = await take(clock, streams);
		await acceptClustered(old);

		republish(origin, broadcast);

		// Draft-18+ withdraws by cancelling the request, never a FIN.
		expect(await old.reader.done().catch(error)).toMatchObject({ code: StreamCode.Cancel });
		const next = await take(clock, streams);
		expect(await acceptClustered(next)).toMatchObject({ cost: 4n });
	} finally {
		close();
	}
});

test("a republish withdraws the solicited NAMESPACE and sends it again", async () => {
	const { pair, pub, origin, close } = clustered(true);
	try {
		const broadcastPublication = origin.createBroadcast(Path.from("mine"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce({ hops: [VIA], cost: 4n });

		const subscription = await Stream.open(pair.client, { version: VERSION });
		const stream = await Stream.accept(pair.server, VERSION);
		if (!stream) throw new Error("the subscription stream was never accepted");
		void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), stream);

		expect(await subscription.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(subscription.reader, VERSION);
		expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
		await SubscribeNamespaceEntry.decode(subscription.reader, VERSION, CLUSTER);

		republish(origin, broadcast);

		expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntryDone.id);
		expect((await SubscribeNamespaceEntryDone.decode(subscription.reader, VERSION)).suffix).toBe(Path.from("mine"));
		expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
		expect((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION, CLUSTER)).suffix).toBe(
			Path.from("mine"),
		);
		subscription.close();
	} finally {
		close();
	}
});

/**
 * A REQUEST_ERROR on an update withdraws the advertisement, and the fresh offer that
 * brings it back waits out the interval the peer named, or never comes for an interval
 * of 0.
 */
test.each([
	["waits out its retry interval", 10_000n],
	["that forbids retrying is never re-offered", 0n],
])("a refused update %s", async (_, retryInterval) => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [VIA], cost: 8n });
		await readUpdate(stream);
		await declinePublishNamespace(stream, retryInterval);

		// Our side cancels too, which completes the withdrawal.
		expect(await stream.reader.done().catch(error)).toMatchObject({ code: StreamCode.Cancel });

		// Well past our own backoff, but inside the interval the peer asked for.
		await clock.advance(Nano.fromMilli(Milli(9_000)));
		expect(streams).toHaveLength(0);

		if (retryInterval === 0n) {
			await clock.advance(Nano.fromMilli(Milli(20_000)));
			expect(streams).toHaveLength(0);
			return;
		}

		// Past the interval, the next backoff (at most its ceiling) brings it back.
		await advanceUntil(clock, () => streams.length > 0, 6_500);
		const fresh = await take(clock, streams);
		expect(await acceptClustered(fresh)).toMatchObject({ cost: 8n });
	} finally {
		close();
	}
});

/**
 * A peer that refuses an update and then closes its side races our own withdrawal. The
 * request is gone either way, so the loop keeps the refusal and re-offers the namespace
 * instead of ending.
 */
test("a refused update whose peer closes the request keeps the loop running", async () => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		let failed: unknown;
		void pub.runPublishNamespaces().catch((err: unknown) => {
			failed = err;
		});

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [VIA], cost: 8n });
		await readUpdate(stream);
		await declinePublishNamespace(stream, 1n);
		stream.close();

		await advanceUntil(clock, () => streams.length > 0 || failed !== undefined, 10_000);
		expect(failed).toBeUndefined();
		const fresh = await take(clock, streams);
		expect(await acceptClustered(fresh)).toMatchObject({ cost: 8n });
	} finally {
		close();
	}
});

/**
 * One update is outstanding per stream, which is what satisfies MAX_REQUEST_UPDATES
 * without reading it: a change landing while one waits for its answer goes out after.
 */
test("a change while an update is unanswered waits for its answer", async () => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [VIA], cost: 8n });
		expect(await readUpdate(stream)).toEqual({ hops: undefined, cost: 8n });

		broadcast.announce({ hops: [VIA], cost: 12n });
		const second = watch(stream.reader.u53());
		await clock.advance(Nano.fromMilli(Milli(1_000)));
		expect(second.state).toBe("pending");
		expect(streams).toHaveLength(0);

		await acceptPublishNamespace(stream);
		await clock.advance(Nano.zero);
		expect(second.state).toBe("resolved");
		expect(second.value).toBe(PublishNamespaceUpdate.id);
		expect((await PublishNamespaceUpdate.decode(stream.reader, VERSION)).update).toEqual({
			hops: undefined,
			cost: 12n,
		});
		await acceptPublishNamespace(stream);
	} finally {
		close();
	}
});

/**
 * From draft 18 an idle advertisement keeps reading, so one the peer ends is offered again.
 * The wait doubles each time, so a peer that ends every advertisement it accepts gets them
 * on a backoff instead of a stream per turn of the loop.
 */
test("an advertisement the peer keeps ending comes back on a growing backoff", async () => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		origin.createBroadcast(Path.from("mine")).announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		let stream = await take(clock, streams);
		for (const wait of [100, 200, 400, 800]) {
			await acceptClustered(stream);
			stream.writer.close();
			await clock.advance(Nano.zero);

			// Just short of the wait, nothing has been offered yet.
			await clock.advance(Nano.fromMilli(Milli(wait - 50)));
			expect(streams).toHaveLength(0);

			await advanceUntil(clock, () => streams.length > 0, 10_000);
			stream = await take(clock, streams);
		}
	} finally {
		close();
	}
});

/**
 * A peer that never answers an update cannot be assumed to hold either price, so the
 * request is dropped and the namespace comes back on a fresh one.
 */
test("an unanswered update drops the request and re-offers the namespace fresh", async () => {
	const { clock, pub, origin, streams, close } = clustered();
	try {
		const broadcast = origin.createBroadcast(Path.from("mine"));
		broadcast.announce({ hops: [VIA], cost: 4n });
		void pub.runPublishNamespaces();

		const stream = await take(clock, streams);
		await acceptClustered(stream);

		broadcast.announce({ hops: [VIA], cost: 8n });
		await readUpdate(stream);
		const reset = watch(stream.reader.u53());

		await clock.advance(Nano.fromMilli(Milli(9_900)));
		expect(reset.state).toBe("pending");
		expect(streams).toHaveLength(0);

		// The timeout resets the request, then the retry offers it on a new one.
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(reset.state).toBe("rejected");
		await advanceUntil(clock, () => streams.length > 0, 200);
		const fresh = await take(clock, streams);
		expect(await acceptClustered(fresh)).toMatchObject({ cost: 8n });
	} finally {
		close();
	}
});

/**
 * MoQ Cluster carries one static cost, and nothing at all without it, so a route change
 * the peer cannot see must not withdraw and advertise the namespace again.
 */
test.each([
	["an unchanged price with Cluster", HopSchema.parse(9n)],
	["any re-price without Cluster", undefined],
])("%s sends nothing", async (_, peer) => {
	using clock = manual();
	const self: Hop = HopSchema.parse(7n);
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const { pub, origin } = publisher(pair.server, { cluster: { self, peer } });
	const broadcast = origin.createBroadcast(Path.from("mine"));
	broadcast.announce({ cost: 4n });
	void pub.runPublishNamespaces();

	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("no PUBLISH_NAMESPACE for the broadcast");
	expect(await stream.reader.u53()).toBe(PublishNamespace.id);
	const msg = await PublishNamespace.decode(stream.reader, VERSION, { cluster: peer !== undefined, epoch: false });
	expect(msg.trackNamespace).toBe(Path.from("mine"));
	await acceptPublishNamespace(stream);

	broadcast.announce({ cost: peer === undefined ? 8n : 4n });
	await noStream(pair.client, clock);

	origin.close();
});

test("subscription completion sends PUBLISH_DONE on every supported draft", async () => {
	const versions = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;

	for (const version of versions) {
		// A revocation bridged from upstream keeps its meaning: UNAUTHORIZED, not INTERNAL_ERROR.
		for (const [abort, status] of [
			[undefined, 0x2],
			[new Error("failed"), 0x0],
			[unauthorized("test"), 0x1],
			[new SessionError(SessionCode.Expired), 0x1],
		] as const) {
			const pair = createMockTransportPair(ALPN.DRAFT_18);
			const session = new NativeSession(pair.server, version, true);
			const path = Path.from("test");
			const { pub, origin } = publisher(pair.server, { session });
			const broadcast = publish(origin, path);
			const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

			const client = await Stream.open(pair.client, { version });
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("publisher never accepted the subscribe stream");

			const requestId = 7n;
			const running = pub.runSubscribe(
				new Subscribe({ requestId, trackNamespace: path, trackName: "video", subscriberPriority: 0 }),
				server,
			);

			expect(await client.reader.u53()).toBe(SubscribeOk.id);
			await SubscribeOk.decode(client.reader, version);
			if (abort) track.abort(abort);
			else {
				track.finish();
				track.close();
			}

			expect(await client.reader.u53()).toBe(PublishDone.id);
			const done = await PublishDone.decode(client.reader, version);
			expect(done.statusCode).toBe(status);

			await running;
			client.close();
			broadcast.close();
			origin.close();
		}
	}
});

/** Draft-22 is the only version whose Location Filters and fills the publisher acts on. */
const V22 = Version.DRAFT_22;

/** The PUBLISH_DONE status for a subscription the track itself ended. */
const TRACK_ENDED_STATUS = 0x2;

/** A group stream the publisher opened, decoded down to its objects. */
interface ServedGroup {
	/** The group's sequence number. */
	sequence: number;
	/** Whether the header claimed the stream starts at the group's first object. */
	firstObject: boolean;
	/** Whether the header claimed the stream's FIN ends the group (END_OF_GROUP). */
	endOfGroup: boolean;
	/** Each object's absolute id (reconstructed from its delta) and payload. */
	objects: { id: number; payload: string }[];
}

/** A fill's fetch stream, decoded down to its objects. */
interface ServedFill {
	/** The request id the FETCH_HEADER named, when it survived a reset. */
	requestId?: bigint;
	/**
	 * Each object's group, absolute id, and payload, and each End of Range's group and id
	 * with `end <reason in hex>` for its payload.
	 */
	objects: { group: number; id: number; payload: string }[];
	/**
	 * The error the stream ended with, when the publisher reset it instead of finishing.
	 *
	 * Carried rather than reduced to a flag so a test can assert *why* the publisher gave
	 * up: a reset arrives here as the reason the publisher chose, so a decoder failure in
	 * this helper cannot pass for one.
	 */
	reset?: Error;
}

/** The subprotocol each draft negotiates, so the mock pair names the version under test. */
const ALPNS: Record<IetfVersion, string> = {
	[Version.DRAFT_18]: ALPN.DRAFT_18,
	[Version.DRAFT_20]: ALPN.DRAFT_20,
	[Version.DRAFT_22]: ALPN.DRAFT_22,
};

/**
 * A publisher serving one broadcast over `version` (draft-22 unless a test says otherwise),
 * with the subscribe stream already open.
 *
 * The uni reader is taken up front: a group stream opened before the test asks for one still
 * queues, but taking the reader late races the publisher rather than the test.
 */
function fixture(
	version: IetfVersion = V22,
	grant?: Signal<Grant | undefined>,
): {
	pair: ReturnType<typeof createMockTransportPair>;
	pub: Publisher;
	broadcast: BroadcastProducer;
	uni: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>;
	version: IetfVersion;
	close: () => void;
} {
	const pair = createMockTransportPair(ALPNS[version]);
	const session = new NativeSession(pair.server, version, false);
	const { pub, origin } = publisher(pair.server, { session, grant });
	const broadcast = publish(origin, Path.from("test"));
	const uni = pair.client.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;

	return {
		pair,
		pub,
		broadcast,
		uni,
		version,
		close: () => {
			uni.releaseLock();
			origin.close();
		},
	};
}

/** Write `frames` numbered payloads into a new closed group. */
function writeGroup(track: TrackProducer, frames: number): void {
	const group = track.appendGroup();
	for (let i = 0; i < frames; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`${group.sequence}.${i}`), timestamp: Timestamp.now() });
	}
	group.finish();
}

/** Send `msg` on a fresh subscribe stream and read the publisher's SUBSCRIBE_OK. */
async function runSubscribe(
	fx: ReturnType<typeof fixture>,
	msg: Subscribe,
): Promise<{ client: Stream; ok: SubscribeOk }> {
	const client = await Stream.open(fx.pair.client, { version: fx.version });
	const server = await Stream.accept(fx.pair.server, fx.version);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	void fx.pub.runSubscribe(msg, server);

	expect(await client.reader.u53()).toBe(SubscribeOk.id);
	return { client, ok: await SubscribeOk.decode(client.reader, fx.version) };
}

/** Take the next uni stream the publisher opened, or undefined if it opened none. */
async function nextUni(
	uni: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>,
): Promise<ReadableStream<Uint8Array> | undefined> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const next = await Promise.race([
			uni.read(),
			new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), STREAM_WAIT);
			}),
		]);
		if (!next || next.done) return undefined;
		return next.value;
	} finally {
		clearTimeout(timer);
	}
}

/** Read a group stream to its end. */
async function readGroup(stream: ReadableStream<Uint8Array> | Reader): Promise<ServedGroup> {
	const reader = stream instanceof Reader ? stream : new Reader(stream, undefined, V22);
	const header = await GroupMessage.decode(reader, V22);

	// Decoded by hand rather than through Frame: a filter that trims a group's head puts the
	// first object's absolute id in the delta, which Frame.decode refuses on principle.
	const objects: { id: number; payload: string }[] = [];
	let id = 0;
	let first = true;
	while (!(await reader.done())) {
		const delta = await reader.u53();
		id = first ? delta : id + delta + 1;
		first = false;
		await reader.read(await reader.u53()); // object properties
		const payload = await reader.read(await reader.u53());
		objects.push({ id, payload: new TextDecoder().decode(payload) });
	}

	return {
		sequence: header.groupId,
		firstObject: header.flags.firstObject,
		endOfGroup: header.flags.hasEnd,
		objects,
	};
}

/** Read a stream carrying only an END_OF_TRACK object, returning the group it names. */
async function readEndOfTrack(stream: ReadableStream<Uint8Array>): Promise<number> {
	const reader = new Reader(stream, undefined, V22);
	const header = await GroupMessage.decode(reader, V22);
	expect(header.flags.hasEnd).toBe(false);
	const frame = await reader.decode((c) => Frame.decode(c, header.flags, undefined));
	expect(frame.endOfTrack).toBe(true);
	expect(await reader.done()).toBe(true);
	return header.groupId;
}

/**
 * Read a fill's fetch stream to its end, reporting a reset rather than throwing.
 *
 * A reset discards data the peer has not acknowledged, so a refused fill may lose its
 * FETCH_HEADER along with the rest: the request id is only reported when it arrived.
 */
async function readFill(
	stream: ReadableStream<Uint8Array> | Reader,
	order: "ascending" | "descending" = "descending",
): Promise<ServedFill> {
	const reader = stream instanceof Reader ? stream : new Reader(stream, undefined, V22);
	const objects: { group: number; id: number; payload: string }[] = [];
	let group = 0;
	let id = 0;

	// Guarded on its own, so the assertion below lands outside every catch. Folding it into
	// the object loop's would report a wrong stream type as a publisher reset.
	let header: { type: number; requestId: bigint } | undefined;
	try {
		const type = await reader.u53();
		header = { type, requestId: (await FetchHeader.decode(reader, V22)).requestId };
	} catch (err) {
		return { objects, reset: error(err) };
	}
	expect(header.type).toBe(FetchHeader.type);
	const requestId = header.requestId;

	try {
		while (!(await reader.done())) {
			const flags = await reader.u53();
			// Absolute ids, and nothing after them.
			if (flags >= 0x80) {
				group = Number(await reader.u62());
				id = Number(await reader.u62());
				objects.push({ group, id, payload: `end ${flags.toString(16)}` });
				continue;
			}
			if (flags & 0x08) {
				const value = await reader.u53();
				group = objects.length === 0 ? value : order === "ascending" ? group + value + 1 : group - value - 1;
			}
			if (flags & 0x04) {
				id = await reader.u53();
			} else {
				id += 1;
			}
			if (flags & 0x10) await reader.u8();
			if (flags & 0x20) await reader.read(await reader.u53());
			const payload = await reader.read(await reader.u53());
			objects.push({ group, id, payload: new TextDecoder().decode(payload) });
		}
	} catch (err) {
		return { requestId, objects, reset: error(err) };
	}

	return { requestId, objects, reset: undefined };
}

/**
 * An absolute filter names the objects it wants, so the boundary groups are trimmed to it
 * and the groups outside it are never opened. The first object written carries its absolute
 * id, or the subscriber would read a silently renumbered group, and a capped tail does not
 * claim END_OF_GROUP, or the subscriber would think the group ended at the cap.
 */
test("draft-22: an absolute filter trims the range it serves", async () => {
	using clock = manual();
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	for (let i = 0; i < 4; i++) writeGroup(track, 3);

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 1n, startObject: 1n, endGroup: 2n, endObject: 0n },
		}),
	);

	try {
		// The request forwarded upstream carries the model's exclusive end, one past the
		// filter's inclusive last group.
		expect(track.subscription.peek()).toMatchObject({
			groups: { start: { included: 1 }, end: { excluded: 3 } },
		});

		const first = await nextUni(fx.uni);
		if (!first) throw new Error("the filter's start group was never served");
		expect(await readGroup(first)).toEqual({
			sequence: 1,
			// The head was trimmed, so the stream does not start at the group's first object.
			firstObject: false,
			endOfGroup: true,
			objects: [
				{ id: 1, payload: "1.1" },
				{ id: 2, payload: "1.2" },
			],
		});

		const second = await nextUni(fx.uni);
		if (!second) throw new Error("the filter's end group was never served");
		expect(await readGroup(second)).toEqual({
			sequence: 2,
			firstObject: true,
			// The filter ends at object 0 of 3, so the stream stops before the group does.
			endOfGroup: false,
			objects: [{ id: 0, payload: "2.0" }],
		});

		// Groups 0 and 3 are outside the range, so nothing more is opened.
		expect(await opens(fx.uni.read(), clock)).toBe(false);
	} finally {
		fx.close();
		client.close();
	}
});

test.each([Version.DRAFT_22])("IETF %s: datagrams honor the absolute object range", async (version) => {
	using _clock = manual();
	const fx = fixture(version);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 1n, endGroup: 1n, endObject: 0n },
		}),
	);
	const reader = fx.pair.client.datagrams.readable.getReader();
	try {
		// Datagrams go out in arrival order, so the in-range one arrives last: receiving it
		// first proves both out-of-range ones ahead of it were dropped.
		for (const sequence of [0, 2, 1])
			track.insertDatagram(sequence, Timestamp.fromMillis(sequence), new Uint8Array([sequence]));
		const next = await reader.read();
		if (next.done) throw new Error("the in-range datagram was never sent");
		const datagram = await ObjectDatagram.decode(next.value, version);
		expect(datagram.trackAlias).toBe(7n);
		expect(datagram.groupId).toBe(1);
		expect(datagram.objectId ?? 0).toBe(0);
	} finally {
		await reader.cancel();
		client.close();
		fx.close();
	}
});

/** An absolute filter has a fixed start, so its SUBSCRIBE is no join even over the refresh ratio. */
test("draft-22: an absolute subscribe is no join", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI, refresh: 0 });
	writeGroup(track, 3);

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
		}),
	);

	try {
		expect(track.demand().wanted.peek()).toBe(0);
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * The draft's own current-group join: a Next Object subscription for the live tail, plus a
 * StartGroup=1 fill for the head already published. The two must meet exactly, so the head
 * arrives once, on the fetch stream, and the subscription picks up at the next object.
 */
test.each(["nextObject", "unfiltered"] as const)(
	"draft-22: a %s fill serves the current group head once",
	async (kind) => {
		const fx = fixture();
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

		writeGroup(track, 1);
		const group = track.appendGroup();
		for (let i = 0; i < 2; i++) {
			group.writeFrame({ payload: new TextEncoder().encode(`1.${i}`), timestamp: Timestamp.now() });
		}

		const { client, ok } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 7n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
				filter: { kind },
				fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
			}),
		);

		try {
			// A fill-requesting subscriber sizes its backfill against this.
			expect(ok.largest).toEqual({ groupId: 1n, objectId: 1n });

			const fill = await nextUni(fx.uni);
			if (!fill) throw new Error("no fetch stream for the requested fill");
			expect(await readFill(fill)).toEqual({
				requestId: 7n,
				objects: [
					{ group: 1, id: 0, payload: "1.0" },
					{ group: 1, id: 1, payload: "1.1" },
				],
				reset: undefined,
			});

			// Everything past the snapshot belongs to the subscription, not the fill.
			group.writeFrame({ payload: new TextEncoder().encode("1.2"), timestamp: Timestamp.now() });
			group.finish();

			const live = await nextUni(fx.uni);
			if (!live) throw new Error("the subscription never served the live tail");
			expect(await readGroup(live)).toEqual({
				sequence: 1,
				firstObject: false,
				endOfGroup: true,
				objects: [{ id: 2, payload: "1.2" }],
			});
		} finally {
			fx.close();
			client.close();
		}
	},
);

test("draft-22: a fill never rewinds an absolute live join", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 2);
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 1n, startObject: 0n },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);
	try {
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing fill before the live join");
		expect((await readFill(fill)).objects).toEqual([
			{ group: 0, id: 0, payload: "0.0" },
			{ group: 0, id: 1, payload: "0.1" },
		]);
		const group = track.appendGroup();
		group.writeFrame({ payload: new TextEncoder().encode("1.0"), timestamp: Timestamp.now() });
		group.finish();
		const live = await nextUni(fx.uni);
		if (!live) throw new Error("missing live group after the fill");
		expect(await readGroup(live)).toEqual({
			sequence: 1,
			firstObject: true,
			endOfGroup: true,
			objects: [{ id: 0, payload: "1.0" }],
		});
	} finally {
		client.close();
		fx.close();
	}
});

/**
 * A group that outgrows its cache is aborted. A Next Object subscriber joining that group
 * sees the abort rather than a live tail served off a trimmed head.
 */
test("draft-22: an open group that outgrew its cache aborts instead of serving a tail", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const group = track.appendGroup();
	for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`0.${i}`), timestamp: Timestamp.now() });
	}
	expect(() =>
		group.writeFrame({ payload: new TextEncoder().encode("overflow"), timestamp: Timestamp.now() }),
	).toThrow();

	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
		}),
	);

	try {
		expect(ok.largest).toEqual({ groupId: 0n, objectId: BigInt(MAX_GROUP_FRAMES - 1) });

		const live = await nextUni(fx.uni);
		if (!live) throw new Error("the subscription never opened a stream for the aborted group");
		await expect(readGroup(live)).rejects.toBeDefined();
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * An absolute filter naming one group with `startObject` above `endObject` selects nothing.
 * Nothing rejects it on the wire, so the serving loop has to recognize the empty range and
 * end the stream, rather than waiting on a start object the range itself excludes.
 */
test("draft-22: a backwards range within one group serves nothing and ends the stream", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

	// Deliberately left open: a hang here would outlive the group rather than end with it.
	const group = track.appendGroup();
	for (let i = 0; i < 3; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`0.${i}`), timestamp: Timestamp.now() });
	}

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 5n, endGroup: 0n, endObject: 2n },
		}),
	);

	try {
		const served = await nextUni(fx.uni);
		if (!served) throw new Error("the group stream never opened");
		expect(await readGroup(served)).toEqual({ sequence: 0, firstObject: false, endOfGroup: false, objects: [] });
	} finally {
		fx.close();
		client.close();
	}
});

test.each(["ascending", "descending"] as const)("draft-22: a wider fill is refused in %s order", async (order) => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	for (const sequence of [0, 3, 9]) {
		const group = track.createGroup(sequence);
		for (let i = 0; i < 2; i++)
			group.writeFrame({ payload: new TextEncoder().encode(`${sequence}.${i}`), timestamp: Timestamp.now() });
		group.finish();
	}

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "unfiltered" }, groupOrder: order, rangeFilters: false },
		}),
	);

	try {
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing fill stream");
		const served = await readFill(fill, order);
		expect(served.reset).toBeDefined();
		expect(served.objects).toEqual([]);
		const live = track.appendGroup();
		live.writeFrame({ payload: new TextEncoder().encode("live"), timestamp: Timestamp.now() });
		live.finish();
		const objects = [];
		while (objects.length === 0) {
			const stream = await nextUni(fx.uni);
			if (!stream) throw new Error("subscription ended with its refused fill");
			objects.push(...(await readGroup(stream)).objects);
		}
		expect(objects).toEqual([{ id: 0, payload: "live" }]);
	} finally {
		fx.close();
		client.close();
	}
});

/** A fill against a track with nothing published has an empty range: no stream is owed. */
test("draft-22: an empty track opens no fill stream", async () => {
	using clock = manual();
	const fx = fixture();
	fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);

	try {
		expect(ok.largest).toBeUndefined();
		expect(await opens(fx.uni.read(), clock)).toBe(false);
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * Subscribe over `version` to a track whose live edge is object 3 of group 5, and report the
 * Largest Location the SUBSCRIBE_OK advertised.
 */
async function subscribeOkLargest(version: IetfVersion): Promise<SubscribeOk["largest"]> {
	const fx = fixture(version);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const group = track.createGroup(5);
	for (let i = 0; i < 4; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`5.${i}`), timestamp: Timestamp.now() });
	}
	group.finish();

	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "unfiltered" },
		}),
	);

	try {
		return ok.largest;
	} finally {
		fx.close();
		client.close();
	}
}

/**
 * LARGEST_OBJECT is the live edge on every draft: a Next Object subscription starts right
 * after it, and the head before it comes back by a fill or a joining FETCH that ends there.
 */
test("LARGEST_OBJECT is the live edge", async () => {
	for (const version of IETF_VERSIONS) {
		expect(await subscribeOkLargest(version)).toEqual({ groupId: 5n, objectId: 3n });
	}
});

/**
 * INCLUDE_PROPERTIES=0 opts the response out of Track Properties, which also opts the track
 * out of timestamps: with no declared Timescale there are no units to read one in.
 */
test("draft-22: an opt-out peer gets no track properties", async () => {
	for (const propertiesWanted of [true, false]) {
		const fx = fixture();
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

		const { client, ok } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 7n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
				propertiesWanted,
			}),
		);

		expect(ok.properties.timescale !== undefined).toBe(propertiesWanted);
		expect(ok.properties.groupOrder !== undefined).toBe(propertiesWanted);

		// With no TIMESCALE declared there are no units to read a timestamp in, so the objects
		// must not carry one either: a bare value invites a peer to read it as some default.
		writeGroup(track, 1);

		const served = await nextUni(fx.uni);
		if (!served) throw new Error("the group was never served");
		const reader = new Reader(served, undefined, V22);
		const header = await GroupMessage.decode(reader, V22);
		expect(header.flags.hasExtensions).toBe(propertiesWanted);

		await reader.u53(); // object id delta
		if (propertiesWanted) {
			const length = await reader.u53();
			expect(length).toBeGreaterThan(0); // the properties block, carrying the timestamp
			await reader.read(length);
		}
		expect(await reader.read(await reader.u53())).toEqual(new TextEncoder().encode("0.0"));

		fx.close();
		client.close();
	}
});

/**
 * Through draft-15 SUBSCRIBE_OK restates the order the subscription is served in; later
 * drafts announce the publisher's default, newest first.
 */
test.each([[Version.DRAFT_18, GroupOrder.Descending]] as const)(
	"%s: SUBSCRIBE_OK announces the served order",
	async (version, announced) => {
		const fx = fixture(version);
		fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

		const { client, ok } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 7n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
				groupOrder: "ascending",
			}),
		);
		expect(ok.properties.groupOrder).toBe(announced);

		fx.close();
		client.close();
	},
);

/** A track with units declares TIMESCALE in SUBSCRIBE_OK and stamps each object. */
test("a timed track declares TIMESCALE and stamps its objects", async () => {
	for (const version of [Version.DRAFT_18] as const) {
		const fx = fixture(version);
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const { client, ok } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 7n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
			}),
		);

		expect(ok.properties.timescale !== undefined).toBe(true);

		writeGroup(track, 1);
		const served = await nextUni(fx.uni);
		if (!served) throw new Error("the group was never served");
		const reader = new Reader(served, undefined, version);
		const header = await GroupMessage.decode(reader, version);
		expect(header.flags.hasExtensions).toBe(true);

		await reader.u53(); // object id delta
		const length = await reader.u53();
		expect(length).toBeGreaterThan(0);
		await reader.read(length);
		expect(await reader.read(await reader.u53())).toEqual(new TextEncoder().encode("0.0"));

		fx.close();
		client.close();
	}
});

test.each([false, true])("draft-22: exact terminal 5/5 survives group FIN=%s", async (finished) => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video");
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
		}),
	);
	try {
		const group = track.createGroup(5);
		for (let i = 0; i < 5; i++) group.writeFrame({ payload: new Uint8Array([i]) });
		const uni = await fx.uni.read();
		if (!uni.value) throw new Error("missing group stream");
		const reader = new Reader(uni.value, undefined, V22);
		const header = await GroupMessage.decode(reader, V22);
		expect(header.groupId).toBe(5);
		for (let i = 0; i < 5; i++) {
			const frame = await reader.decode((c) => Frame.decode(c, header.flags, undefined));
			expect(frame.payload).toEqual(new Uint8Array([i]));
		}
		if (finished) {
			group.finish();
			expect(await reader.done()).toBe(true);
		}
		track.finishAt({ group: 5, frame: 5 });
		group.finish();
		track.finish();
		track.close();
		if (finished) {
			const marker = await fx.uni.read();
			if (!marker.value) throw new Error("missing terminal stream");
			const end = new Reader(marker.value, undefined, V22);
			const endHeader = await GroupMessage.decode(end, V22);
			expect(endHeader.groupId).toBe(5);
			expect(endHeader.flags.hasEnd).toBe(false);
			expect(endHeader.flags.firstObject).toBe(false);
			const frame = await end.decode((c) => Frame.decode(c, endHeader.flags, undefined));
			expect(frame.endOfTrack).toBe(true);
			expect(frame.idDelta).toBe(5);
			expect(await end.done()).toBe(true);
		} else {
			const frame = await reader.decode((c) => Frame.decode(c, header.flags, undefined));
			expect(frame.endOfTrack).toBe(true);
			expect(frame.idDelta).toBe(0);
			expect(await reader.done()).toBe(true);
		}
		expect(await client.reader.u53()).toBe(PublishDone.id);
		expect((await PublishDone.decode(client.reader, V22)).streamCount).toBe(finished ? 2n : 1n);
	} finally {
		fx.close();
		client.close();
	}
});

/** An inline END_OF_TRACK on a stream reset after its FIN may never arrive, so it is resent. */
test("draft-22: an inline terminal reset after its FIN falls back to a standalone marker", async () => {
	const opened: Writer[] = [];
	const tryOpen = Writer.tryOpen.bind(Writer);
	const spy = spyOn(Writer, "tryOpen").mockImplementation(async (...args) => {
		const writer = await tryOpen(...args);
		if (writer) opened.push(writer);
		return writer;
	});
	const fx = fixture();
	const track = fx.broadcast.createTrack("video");
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
		}),
	);
	try {
		const group = track.createGroup(5);
		for (let i = 0; i < 5; i++) group.writeFrame({ payload: new Uint8Array([i]) });
		const uni = await fx.uni.read();
		if (!uni.value) throw new Error("missing group stream");
		const reader = new Reader(uni.value, undefined, V22);
		const header = await GroupMessage.decode(reader, V22);
		for (let i = 0; i < 5; i++) await reader.decode((c) => Frame.decode(c, header.flags, undefined));

		// The group stream's close fails once it FINs, like a reset after its FIN.
		const closed = Object.getOwnPropertyDescriptor(Writer.prototype, "closed")?.get;
		if (!closed) throw new Error("Writer.closed is not a getter");
		const stream = opened[0];
		const fin = stream.close.bind(stream);
		let reset = false;
		stream.close = () => {
			reset = true;
			fin();
		};
		Object.defineProperty(stream, "closed", {
			get(this: Writer) {
				return reset ? Promise.reject(new Error("reset after FIN")) : closed.call(this);
			},
		});

		track.finishAt({ group: 5, frame: 5 });
		group.finish();
		track.finish();
		track.close();

		const inline = await reader.decode((c) => Frame.decode(c, header.flags, undefined));
		expect(inline.endOfTrack).toBe(true);
		const marker = await fx.uni.read();
		if (!marker.value) throw new Error("missing standalone terminal stream");
		const end = new Reader(marker.value, undefined, V22);
		const endHeader = await GroupMessage.decode(end, V22);
		expect(endHeader.groupId).toBe(5);
		const frame = await end.decode((c) => Frame.decode(c, endHeader.flags, undefined));
		expect(frame.endOfTrack).toBe(true);
		expect(frame.idDelta).toBe(5);
		expect(await client.reader.u53()).toBe(PublishDone.id);
		expect((await PublishDone.decode(client.reader, V22)).streamCount).toBe(2n);
	} finally {
		spy.mockRestore();
		fx.close();
		client.close();
	}
});

/**
 * A bounded filter does not end the subscription (draft-22 has none), so the publisher
 * keeps serving until the track does. Groups held above the end cannot delay PUBLISH_DONE
 * once the track finishes: the filter is fixed, so its terminal boundary ends serving.
 */
test("draft-22: a clean close past a bounded filter's end still sends PUBLISH_DONE", async () => {
	using clock = manual();
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1); // group 0, the whole requested range

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 0n, endGroup: 0n },
		}),
	);

	try {
		const served = await nextUni(fx.uni);
		if (!served) throw new Error("the requested group was never served");
		expect((await readGroup(served)).sequence).toBe(0);

		// Beyond the end, so it is never served, and it must not hold the subscription open.
		writeGroup(track, 1); // group 1
		track.finish();
		track.close();

		expect(await client.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(client.reader, V22);
		expect(done.statusCode).toBe(TRACK_ENDED_STATUS);
		expect(done.streamCount).toBe(2n);

		// Only the in-range group was ever served; the other stream marks the track's end.
		const end = await nextUni(fx.uni);
		if (!end) throw new Error("the track's end was never marked");
		expect(await readEndOfTrack(end)).toBe(2);
		expect(await opens(fx.uni.read(), clock)).toBe(false);
	} finally {
		fx.close();
		client.close();
	}
});

test.each([0, 1000])("draft-22: FILL_TIMEOUT=%s is ignored for a largest-group fill", async (timeout) => {
	using _clock = manual();
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const open = track.appendGroup();
	open.writeFrame({ payload: new TextEncoder().encode("0.0"), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 1n }, fillTimeout: Milli(timeout), rangeFilters: false },
		}),
	);
	try {
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing fill");
		expect(await readFill(fill)).toMatchObject({ objects: [{ group: 0, id: 0, payload: "0.0" }] });
	} finally {
		open.finish();
		fx.close();
		client.close();
	}
});

/**
 * A broadcast served through `requested()` resolves its track on demand, and a dynamic serve
 * is deliberately one request per peer subscription. Resolving the track again to read the
 * fill's cache would mint a second producer nobody has accepted, so the fill has to read the
 * one already serving the subscription.
 */
test("draft-22: a fill works on a dynamically requested track", async () => {
	const fx = fixture();

	// Answer the request the subscription raises, the way an application serving on demand
	// does, rather than inserting the track up front.
	const serving = (async () => {
		const request = await wireOf(fx.broadcast).requested();
		if (!request) throw new Error("no track was requested");
		const track = request.accept({ timescale: Timescale.MILLI });
		const group = track.appendGroup();
		for (let i = 0; i < 2; i++) {
			group.writeFrame({ payload: new TextEncoder().encode(`0.${i}`), timestamp: Timestamp.now() });
		}
		return group;
	})();

	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);
	const group = await serving;

	try {
		expect(ok.largest).toEqual({ groupId: 0n, objectId: 1n });

		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("no fetch stream for the requested fill");
		expect(await readFill(fill)).toEqual({
			requestId: 7n,
			objects: [
				{ group: 0, id: 0, payload: "0.0" },
				{ group: 0, id: 1, payload: "0.1" },
			],
			reset: undefined,
		});
	} finally {
		group.finish();
		fx.close();
		client.close();
	}
});

// A failed group stream must release that group while the subscription serves later ones.
// Chromium can throw synchronously after closure; other transports reject their open promise.
for (const synchronous of [true, false]) {
	test(`a ${synchronous ? "thrown" : "rejected"} group open does not escape the subscription`, async () => {
		using clock = manual();
		const fx = fixture();
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const attempted = Promise.withResolvers<void>();
		const failure = new Error("stream opening failed");
		const create = spyOn(fx.pair.server, "createUnidirectionalStream").mockImplementationOnce(() => {
			attempted.resolve();
			if (synchronous) throw failure;
			return Promise.reject(failure);
		});
		const { client } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 7n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 0,
				filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
			}),
		);

		try {
			writeGroup(track, 1);
			await attempted.promise;
			await clock.advance(Nano.zero);
			writeGroup(track, 1);

			const next = await fx.uni.read();
			if (next.done) throw new Error("subscription ended after its first group failed");
			const served = await readGroup(next.value);
			expect(served.sequence).toBe(1);
			expect(served.objects).toEqual([{ id: 0, payload: "1.0" }]);
			expect(create).toHaveBeenCalledTimes(2);
		} finally {
			client.close();
			fx.close();
			await clock.advance(Nano.zero);
		}
	});
}

// PUBLISH_DONE MUST wait until every stream the subscription will open is closed, so its
// Stream Count is final. A group still queued for a stream slot when the track ends is one.
test("draft-22: PUBLISH_DONE waits for a queued group and counts every stream", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });

	// Park the first stream open, the way a transport at its stream cap does.
	const slot = Promise.withResolvers<void>();
	const create = fx.pair.server.createUnidirectionalStream.bind(fx.pair.server);
	let parked = false;
	fx.pair.server.createUnidirectionalStream = async (options?: WebTransportSendStreamOptions) => {
		if (!parked) {
			parked = true;
			await slot.promise;
		}
		return create(options);
	};

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 0n, startObject: 0n },
		}),
	);

	try {
		writeGroup(track, 1);
		track.finish();
		track.close();

		// Nothing ends the subscription while the group waits for its slot.
		const response = client.reader.u53();
		const idle = new Promise<"pending">((resolve) => setTimeout(() => resolve("pending"), 20));
		expect(await Promise.race([response, idle])).toBe("pending");

		slot.resolve();
		const served = await nextUni(fx.uni);
		if (!served) throw new Error("the queued group was never served");
		expect((await readGroup(served)).sequence).toBe(0);

		expect(await response).toBe(PublishDone.id);
		const done = await PublishDone.decode(client.reader, V22);
		expect(done.statusCode).toBe(TRACK_ENDED_STATUS);
		// The group's stream and the END_OF_TRACK marker's.
		expect(done.streamCount).toBe(2n);

		const end = await nextUni(fx.uni);
		if (!end) throw new Error("the track's end was never marked");
		expect(await readEndOfTrack(end)).toBe(1);
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * A peer that did not send SOLICIT still gets NAMESPACE on its SUBSCRIBE_NAMESPACE
 * stream on draft-16 and later: one for a match that already exists, one announced
 * after, then NAMESPACE_DONE when that announcement ends.
 */
test.each([["draft-18", Version.DRAFT_18]] as const)(
	"a non-SOLICIT %s SUBSCRIBE_NAMESPACE carries NAMESPACE then NAMESPACE_DONE",
	async (_, version) => {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const session = new NativeSession(pair.server, version, true);
		const { pub, origin } = publisher(pair.server, { requiresSolicitation: false, session });
		const early = publish(origin, Path.from("early-cam"));

		const subscription = await Stream.open(pair.client, { version });
		const accepted = await Stream.accept(pair.server, version);
		if (!accepted) throw new Error("missing subscription");
		const run = pub.runSubscribeNamespace(
			new SubscribeNamespace({ requestId: 1n, namespace: Path.empty() }),
			accepted,
		);

		try {
			expect(await subscription.reader.u53()).toBe(RequestOk.id);
			await RequestOk.decode(subscription.reader, version);

			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			expect((await SubscribeNamespaceEntry.decode(subscription.reader, version)).suffix).toBe(
				Path.from("early-cam"),
			);

			publish(origin, Path.from("late-cam"));
			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			expect((await SubscribeNamespaceEntry.decode(subscription.reader, version)).suffix).toBe(
				Path.from("late-cam"),
			);

			early.close();
			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntryDone.id);
			expect((await SubscribeNamespaceEntryDone.decode(subscription.reader, version)).suffix).toBe(
				Path.from("early-cam"),
			);
		} finally {
			subscription.close();
			origin.close();
			await run;
		}
	},
);

for (const version of [Version.DRAFT_18] as const) {
	for (const declared of [false, true]) {
		for (const solicited of [false, true]) {
			for (const optIn of [false, true]) {
				test(`hidden negotiation: ${version}, declared=${declared}, solicited=${solicited}, optIn=${optIn}`, async () => {
					const pair = createMockTransportPair(ALPN.DRAFT_18);
					const origin = new OriginProducer();
					// Authorization heads must not replace the request's empty visibility prefix.
					const scope = new Path.Patterns([
						Path.Pattern.subtree(Path.from(".stats")),
						Path.Pattern.subtree(Path.from("visible")),
					]);
					const pub = new Publisher({
						quic: pair.server,
						session: new NativeSession(pair.server, version, true),
						publish: origin.scope(Path.empty(), scope).consume(),
						requiresSolicitation: solicited,
						hidden: declared,
					});
					publish(origin, Path.from(".stats/node"));
					publish(origin, Path.from("visible"));
					let subscription: Stream | undefined;
					let run: Promise<void>;
					if (solicited) {
						subscription = await Stream.open(pair.client, { version: version });
						const accepted = await Stream.accept(pair.server, version);
						if (!accepted) throw new Error("missing subscription");
						run = pub.runSubscribeNamespace(
							new SubscribeNamespace({ requestId: 0n, namespace: Path.empty(), hidden: optIn }),
							accepted,
						);
						expect(await subscription.reader.u53()).toBe(RequestOk.id);
						await RequestOk.decode(subscription.reader, version);
					} else {
						run = pub.runPublishNamespaces();
					}
					const expected = !declared || (solicited && optIn) ? [".stats/node", "visible"] : ["visible"];
					const advertisements: Stream[] = [];
					for (const path of expected) {
						if (subscription && true) {
							expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
							expect((await SubscribeNamespaceEntry.decode(subscription.reader, version)).suffix).toBe(
								Path.from(path),
							);
						} else {
							const stream = await Stream.accept(pair.client, version);
							if (!stream) throw new Error("missing advertisement");
							advertisements.push(stream);
							expect(await stream.reader.u53()).toBe(PublishNamespace.id);
							const msg = await PublishNamespace.decode(stream.reader, version);
							expect(msg.trackNamespace).toBe(Path.from(path));
							await stream.writer.u53(RequestOk.id);
							await new RequestOk({}).encode(stream.writer, version);
						}
					}
					subscription?.close();
					origin.close();
					await run;
					for (const stream of advertisements) stream.close();
				});
			}
		}
	}
}

test("requester FIN keeps a draft-22 subscription serving; STOP_SENDING cancels it", async () => {
	const fx = fixture(Version.DRAFT_22);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const client = await Stream.open(fx.pair.client, { version: fx.version });
	const server = await Stream.accept(fx.pair.server, fx.version);
	if (!server) throw new Error("missing stream");
	const serving = fx.pub.runSubscribe(
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
		}),
		server,
	);
	try {
		expect(await client.reader.u53()).toBe(SubscribeOk.id);
		await SubscribeOk.decode(client.reader, fx.version);
		client.writer.close();
		await client.writer.closed;
		writeGroup(track, 1);
		const next = await fx.uni.read();
		expect(next.done).toBeFalse();
		if (next.done) throw new Error("subscription stopped after requester FIN");
		const reader = new Reader(next.value, undefined, fx.version);
		const header = await GroupMessage.decode(reader, fx.version);
		expect(header.groupId).toBe(0);
		client.reader.stop(new Error("cancel"));
		await serving;
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

for (const version of [Version.DRAFT_18] as const) {
	test(`requester FIN cancels a ${version.toString(16)} subscription`, async () => {
		const fx = fixture(version);
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const client = await Stream.open(fx.pair.client, { version: fx.version });
		const server = await Stream.accept(fx.pair.server, fx.version);
		if (!server) throw new Error("missing stream");
		const serving = fx.pub.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 128,
			}),
			server,
		);
		try {
			expect(await client.reader.u53()).toBe(SubscribeOk.id);
			await SubscribeOk.decode(client.reader, fx.version);
			client.writer.close();
			await serving;
			expect(track.subscription.peek()).toBeUndefined();
		} finally {
			client.close();
			track.finish();
			track.close();
			fx.close();
		}
	});
}

test("REQUEST_UPDATE applies priority and preserves it when omitted", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
		}),
	);
	try {
		const before = track.subscription.peek();
		await client.writer.write(new Uint8Array([0x02, 0, 4, 2, 1, 0x20, 10]));
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(track.subscription.peek()?.priority).toBe(117);
		// Only the priority changes; retention and the group range survive.
		expect(track.subscription.peek()?.maxDelay).toBe(before?.maxDelay);
		expect(track.subscription.peek()?.groups).toEqual(before?.groups);
		await client.writer.write(new Uint8Array([0x02, 0, 2, 4, 0]));
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(track.subscription.peek()?.priority).toBe(117);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("REQUEST_UPDATE accepts delivery timeouts without changing the subscription", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
		}),
	);
	try {
		const before = track.subscription.peek();
		await client.writer.write(new Uint8Array([0x02, 0, 6, 2, 2, 0x02, 7, 0x04, 9]));
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(track.subscription.peek()).toEqual(before);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("TRACK_PROPERTY_FILTER on a subscription update closes with PROTOCOL_VIOLATION", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const client = await Stream.open(fx.pair.client, { version: fx.version });
	const server = await Stream.accept(fx.pair.server, fx.version);
	if (!server) throw new Error("missing subscribe stream");
	const serving = fx.pub
		.runSubscribe(
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 128,
			}),
			server,
		)
		.then(
			() => undefined,
			(err: unknown) => err,
		);
	try {
		expect(await client.reader.u53()).toBe(SubscribeOk.id);
		await SubscribeOk.decode(client.reader, fx.version);
		await client.writer.write(new Uint8Array([0x02, 0, 6, 2, 1, 0x29, 2, 0, 0]));
		const failure = await serving;
		expect(failure).toBeInstanceOf(ProtocolViolation);
		expect(Number((failure as ProtocolViolation).code)).toBe(0x03);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
		await serving;
	}
});

test("unsupported REQUEST_UPDATE is refused and ends with UPDATE_FAILED", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
		}),
	);
	try {
		// NEW_GROUP_REQUEST (0x32), on a track without DYNAMIC_GROUPS.
		await client.writer.write(new Uint8Array([0x02, 0, 4, 2, 1, 0x32, 0]));
		expect(await client.reader.u53()).toBe(RequestError.id);
		const refusal = await RequestError.decode(client.reader, fx.version);
		expect(refusal.errorCode).toBe(0x03);
		expect(await client.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(client.reader, fx.version);
		expect(done.statusCode).toBe(0x08);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

// FORWARD=0 drops the subscription, and its upstream demand, while the request stays; FORWARD=1
// subscribes again as a new SUBSCRIBE would, at the live edge.
test("a SUBSCRIBE pauses without demand and resumes as a new subscriber", async () => {
	for (const startPaused of [false, true]) {
		const fx = fixture(V22);
		const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const write = (text: string) => {
			const group = track.appendGroup();
			group.writeFrame({ payload: new TextEncoder().encode(text), timestamp: Timestamp.now() });
			group.finish();
		};
		const { client } = await runSubscribe(
			fx,
			new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("test"),
				trackName: "video",
				subscriberPriority: 128,
				filter: { kind: "nextObject" },
				forward: !startPaused,
			}),
		);
		const forward = async (requestId: bigint, on: boolean) => {
			const parameters = new Parameters();
			parameters.forward = on;
			await client.writer.u53(SubscribeUpdate.id);
			await new SubscribeUpdate({ requestId, parameters }).encode(client.writer, V22);
			expect(await client.reader.u53()).toBe(RequestOk.id);
			return RequestOk.decode(client.reader, V22);
		};
		try {
			write("before");
			if (!startPaused) {
				const before = await nextUni(fx.uni);
				if (!before) throw new Error("the subscription served nothing");
				expect((await readGroup(before)).objects).toEqual([{ id: 0, payload: "before" }]);
				await forward(2n, false);
			}
			while (track.subscription.peek() !== undefined) await track.subscription.changed();
			write("paused");

			const resumed = await forward(4n, true);
			expect(resumed.parameters.largest).toEqual({ groupId: 1n, objectId: 0n });
			write("after");
			for (;;) {
				const stream = await nextUni(fx.uni);
				if (!stream) throw new Error("the resumed subscription served nothing");
				const group = await readGroup(stream);
				const payloads = group.objects.map((object) => object.payload);
				expect(payloads).not.toContain("paused");
				expect(payloads).not.toContain("before");
				if (group.sequence === 2) {
					expect(group.objects).toEqual([{ id: 0, payload: "after" }]);
					break;
				}
			}
		} finally {
			client.close();
			track.finish();
			track.close();
			fx.close();
		}
	}
});

test("a paused SUBSCRIBE ends once its grant is revoked", async () => {
	const grant = new Signal<Grant | undefined>(undefined);
	const fx = fixture(V22, grant);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
			filter: { kind: "nextObject" },
			forward: false,
		}),
	);
	try {
		grant.set({ publish: new Path.Patterns([]), subscribe: new Path.Patterns([]) });
		expect(await client.reader.u53()).toBe(PublishDone.id);
		expect((await PublishDone.decode(client.reader, V22)).statusCode).toBe(PublishDoneStatus.UNAUTHORIZED);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

// The track has no max age, so only the pause itself can release a write parked on flow control.
test("a pause resets a group stream parked on flow control", async () => {
	const fx = fixture(V22);
	let writing!: () => void;
	const parked = new Promise<void>((resolve) => {
		writing = resolve;
	});
	let reset = false;
	const stalled = new WritableStream<Uint8Array>({
		start(controller) {
			controller.signal.addEventListener("abort", () => {
				reset = true;
			});
		},
		write() {
			writing();
			return new Promise<void>(() => {});
		},
	});
	spyOn(fx.pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => stalled);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
			filter: { kind: "nextObject" },
		}),
	);
	try {
		track.appendGroup().writeFrame({ payload: new TextEncoder().encode("parked"), timestamp: Timestamp.now() });
		await parked;
		const parameters = new Parameters();
		parameters.forward = false;
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, V22);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		expect(reset).toBe(true);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("namespace subscription survives requester FIN and stops on STOP_SENDING", async () => {
	const version = Version.DRAFT_22;
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const { pub, origin } = publisher(pair.server, {
		session: new NativeSession(pair.server, version, false),
		requiresSolicitation: true,
	});
	const client = await Stream.open(pair.client, { version: version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("missing namespace request stream");
	const running = pub.runSubscribeNamespace(
		new SubscribeNamespace({ requestId: 0n, namespace: Path.from("") }),
		server,
	);
	try {
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, version);
		client.writer.close();
		await client.writer.closed;
		const broadcast = publish(origin, Path.from("after-fin"));
		expect(await client.reader.u53()).toBe(SubscribeNamespaceEntry.id);
		expect((await SubscribeNamespaceEntry.decode(client.reader, version)).suffix).toBe(Path.from("after-fin"));
		client.reader.stop(new Error("cancel"));
		await running;
		broadcast.close();
	} finally {
		client.close();
		origin.close();
	}
});

test("namespace updates move the prefix after acknowledgement", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const pair = createMockTransportPair(ALPNS[version]);
		const { pub, origin } = publisher(pair.server, {
			session: new NativeSession(pair.server, version, false),
			requiresSolicitation: true,
		});
		const old = publish(origin, Path.from("old/first"));
		const fresh = publish(origin, Path.from("new/second"));
		const client = await Stream.open(pair.client, { version });
		const server = await Stream.accept(pair.server, version);
		if (!server) throw new Error("missing namespace stream");
		const running = pub.runSubscribeNamespace(
			new SubscribeNamespace({ requestId: 0n, namespace: Path.from("old") }),
			server,
		);
		try {
			expect(await client.reader.u53()).toBe(RequestOk.id);
			await RequestOk.decode(client.reader, version);
			expect(await client.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			expect((await SubscribeNamespaceEntry.decode(client.reader, version)).suffix).toBe(Path.from("first"));
			await client.writer.write(Uint8Array.from([0x02, 0, 8, 2, 1, 0x34]));
			// Finishing the same frame must apply it exactly once.
			await client.writer.write(Uint8Array.from([1, 3, 110, 101, 119]));
			expect(await client.reader.u53()).toBe(SubscribeNamespaceEntryDone.id);
			expect((await SubscribeNamespaceEntryDone.decode(client.reader, version)).suffix).toBe(Path.from("first"));
			expect(await client.reader.u53()).toBe(RequestOk.id);
			await RequestOk.decode(client.reader, version);
			expect(await client.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			expect((await SubscribeNamespaceEntry.decode(client.reader, version)).suffix).toBe(Path.from("second"));
			if (version >= Version.DRAFT_20) {
				await client.writer.write(Uint8Array.from([0x02, 0, 6, 4, 1, 0x34, 1, 1, 46]));
				expect(await client.reader.u53()).toBe(RequestError.id);
				expect((await RequestError.decode(client.reader, version)).errorCode).toBe(0x10);
				await running;
			}
		} finally {
			client.close();
			origin.close();
			old.close();
			fresh.close();
			await running;
		}
	}
});

/** MoQ Active Count: the REQUEST_OK counts the NAMESPACEs sent ahead of any change. */
test("a counted answer names the NAMESPACEs that follow it", async () => {
	for (const activeCount of [true, false]) {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const { pub, origin } = publisher(pair.server, { requiresSolicitation: true, activeCount });
		publish(origin, Path.from("a"));
		publish(origin, Path.from("b"));

		const subscription = await Stream.open(pair.client, { version: VERSION });
		const accepted = await Stream.accept(pair.server, VERSION);
		if (!accepted) throw new Error("the subscription stream was never accepted");
		void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), accepted);

		expect(await subscription.reader.u53()).toBe(RequestOk.id);
		const ok = await RequestOk.decode(subscription.reader, VERSION);
		expect(ok.parameters.activeCount).toBe(activeCount ? 2 : undefined);
		const suffixes = [];
		for (let i = 0; i < 2; i++) {
			expect(await subscription.reader.u53()).toBe(SubscribeNamespaceEntry.id);
			suffixes.push((await SubscribeNamespaceEntry.decode(subscription.reader, VERSION)).suffix);
		}
		expect(suffixes.sort()).toEqual([Path.from("a"), Path.from("b")]);

		subscription.close();
		origin.close();
	}
});

/** ACTIVE_COUNT only answers SUBSCRIBE_NAMESPACE, so one on any other REQUEST_OK is a violation. */
test.each([0, Number.MAX_SAFE_INTEGER + 2])(
	"an ACTIVE_COUNT of %p answering a PUBLISH_NAMESPACE closes the session",
	async (count) => {
		const pair = createMockTransportPair(ALPN.DRAFT_18);
		const { pub, origin } = publisher(pair.server, { activeCount: true });
		const warn = spyOn(console, "warn").mockImplementation(() => void 0);
		onTestFinished(() => warn.mockRestore());
		void pub.runPublishNamespaces();
		publish(origin, Path.from("a"));

		const stream = await nextStream(pair.client);
		if (!stream) throw new Error("no PUBLISH_NAMESPACE was sent");
		expect(await readPublishNamespace(stream)).toBe(Path.from("a"));
		const parameters = new Parameters();
		parameters.activeCount = count;
		await stream.writer.u53(RequestOk.id);
		await new RequestOk({ parameters }).encode(stream.writer, VERSION);

		const closed = await Promise.race([
			pair.server.closed,
			new Promise<never>((_resolve, reject) => setTimeout(() => reject(new Error("session stayed up")), 500)),
		]);
		expect(closed.closeCode).toBe(new ProtocolViolation("").code);
		origin.close();
	},
);

test("draft-22: a range update replaces the live subscription bounds", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
		}),
	);
	try {
		const parameters = new Parameters();
		parameters.subscriptionFilter = {
			kind: "absolute",
			startGroup: 2n,
			startObject: 1n,
			endGroup: 3n,
			endObject: 0n,
		};
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		for (let i = 0; i < 5; i++) writeGroup(track, 2);
		track.finish();
		track.close();
		const served: ServedGroup[] = [];
		for (let i = 0; i < 2; i++) {
			const stream = await nextUni(fx.uni);
			if (!stream) throw new Error("missing group from updated range");
			served.push(await readGroup(stream));
		}
		expect(
			served.sort((a, b) => a.sequence - b.sequence).map(({ sequence, objects }) => ({ sequence, objects })),
		).toEqual([
			{ sequence: 2, objects: [{ id: 1, payload: "2.1" }] },
			{ sequence: 3, objects: [{ id: 0, payload: "3.0" }] },
		]);
		expect(await client.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(client.reader, fx.version);
		expect(done.statusCode).toBe(TRACK_ENDED_STATUS);
		expect(done.streamCount).toBe(3n);
		const end = await nextUni(fx.uni);
		if (!end) throw new Error("missing exact track end after updated bounds");
		expect(await readEndOfTrack(end)).toBe(5);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("IETF draft-18: range updates preserve the shared subscription", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
		}),
	);
	try {
		for (const id of [2n, 4n]) {
			const parameters = new Parameters();
			if (id === 2n)
				parameters.subscriptionFilter = { kind: "absolute", startGroup: 6n, startObject: 0n, endGroup: 7n };
			await client.writer.u53(SubscribeUpdate.id);
			await new SubscribeUpdate({ requestId: id, parameters }).encode(client.writer, fx.version);
			expect(await client.reader.u53()).toBe(RequestOk.id);
			await RequestOk.decode(client.reader, fx.version);
			expect(track.subscription.peek()?.groups).toEqual({ start: { included: 6 }, end: { excluded: 8 } });
		}
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: an update fills its snapshot under its own request ID", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	for (let i = 0; i < 3; i++) writeGroup(track, 2);
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 3n, startObject: 0n },
		}),
	);
	try {
		const parameters = new Parameters();
		parameters.subscriptionFilter = { kind: "nextObject" };
		parameters.fillParameters = Filter.encodeFill(
			{ filter: { kind: "relative", groups: 1n }, groupOrder: "ascending", rangeFilters: false },
			fx.version,
		);
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		const ok = await RequestOk.decode(client.reader, fx.version);
		expect(ok.parameters.largest).toEqual({ groupId: 2n, objectId: 1n });
		// The live tail is an independent stream, so find the fill by its header.
		let served: ServedFill | undefined;
		for (let i = 0; i < 2; i++) {
			const stream = await nextUni(fx.uni);
			if (!stream) throw new Error("missing update data stream");
			const reader = new Reader(stream, undefined, V22);
			if ((await reader.peekU62()) === BigInt(FetchHeader.type)) served = await readFill(reader, "ascending");
			else expect((await readGroup(reader)).objects).toEqual([]);
		}
		if (!served) throw new Error("missing update fill");
		expect(served.requestId).toBe(2n);
		expect(served.reset).toBeUndefined();
		expect(served.objects).toEqual(
			[2].flatMap((group) => [0, 1].map((id) => ({ group, id, payload: `${group}.${id}` }))),
		);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: an empty newer group is not an object in Largest or a relative fill", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 2);
	const empty = track.appendGroup();
	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 2n, startObject: 0n },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);
	try {
		expect(ok.largest).toEqual({ groupId: 0n, objectId: 1n });
		expect(track.largest()).toEqual({ group: 1, frame: 0 });
		const stream = await nextUni(fx.uni);
		if (!stream) throw new Error("missing relative fill");
		const served = await readFill(stream);
		expect(served.reset).toBeUndefined();
		expect(served.objects).toEqual([0, 1].map((id) => ({ group: 0, id, payload: `0.${id}` })));
	} finally {
		client.close();
		empty.finish();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: reported Largest survives absent headers, older cache entries, and eviction", async () => {
	using clock = manual();
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	hooks.reportedLargest(track, { group: 4, frame: 3 });
	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 6n, startObject: 0n },
		}),
	);
	try {
		expect(ok.largest).toEqual({ groupId: 4n, objectId: 3n });
		writeGroup(track, 2);
		const empty = track.appendGroup();
		await clock.advance(Nano.fromMilli(Milli(60_000)));
		expect(hooks.cachedLargest(track)).toBeUndefined();
		const parameters = new Parameters();
		parameters.subscriptionFilter = { kind: "absolute", startGroup: 6n, startObject: 0n };
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		expect((await RequestOk.decode(client.reader, fx.version)).parameters.largest).toEqual({
			groupId: 4n,
			objectId: 3n,
		});
		empty.finish();
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: Next Object behind an upstream group report still delivers its first object", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// What an IETF SUBSCRIBE_OK leaves on a cold copy: the group, but no frame it can name.
	hooks.reportedGroup(track, 0);
	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);
	const group = track.appendGroup();
	try {
		expect(ok.largest).toBeUndefined();
		expect(group.sequence).toBe(0);
		group.writeFrame({ payload: new TextEncoder().encode("first"), timestamp: Timestamp.now() });
		group.finish();
		const stream = await nextUni(fx.uni);
		if (!stream) throw new Error("missing first received object");
		expect((await readGroup(stream)).objects).toEqual([{ id: 0, payload: "first" }]);
	} finally {
		client.close();
		group.finish();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: Next Object on an empty group still delivers its first object", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = track.appendGroup();
	const { client, ok } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		}),
	);
	try {
		expect(ok.largest).toBeUndefined();
		group.writeFrame({ payload: new TextEncoder().encode("first"), timestamp: Timestamp.now() });
		group.finish();
		const stream = await nextUni(fx.uni);
		if (!stream) throw new Error("missing first published object");
		expect((await readGroup(stream)).objects).toEqual([{ id: 0, payload: "first" }]);
	} finally {
		client.close();
		group.finish();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: cancellation releases a fill parked on flow control", async () => {
	const fx = fixture();
	const parked = Promise.withResolvers<void>();
	let reset = false;
	const stalled = new WritableStream<Uint8Array>({
		start(controller) {
			controller.signal.addEventListener("abort", () => {
				reset = true;
			});
		},
		write() {
			parked.resolve();
			return new Promise<void>(() => {});
		},
	});
	spyOn(fx.pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => stalled);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1);
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "absolute", startGroup: 10n, startObject: 0n },
			fill: { filter: { kind: "unfiltered" }, rangeFilters: false },
		}),
	);
	try {
		await parked.promise;
		client.close();
		while (track.subscription.peek() !== undefined) await track.subscription.changed();
		expect(reset).toBe(true);
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: narrowing gives up an excluded group's open stream", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const open = track.appendGroup();
	open.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({ requestId: 0n, trackNamespace: Path.from("test"), trackName: "video", subscriberPriority: 0 }),
	);
	try {
		const stream = await nextUni(fx.uni);
		if (!stream) throw new Error("missing initial open group");
		const [data, observed] = stream.tee();
		const reading = readGroup(data).then(() => undefined, error);
		const reader = new Reader(observed, undefined, V22);
		const header = await GroupMessage.decode(reader, V22);
		expect((await reader.decode((body) => Frame.decode(body, header.flags, Timescale.MILLI))).payload).toEqual(
			new Uint8Array([1]),
		);
		reader.stop(new Error("observation complete"));
		const parameters = new Parameters();
		parameters.subscriptionFilter = { kind: "absolute", startGroup: 1n, startObject: 0n };
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(await reading).toMatchObject({ code: StreamCode.Cancel });
		// The publisher remains usable; only this subscription stopped its old group.
		open.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.now() });
		writeGroup(track, 1);
		const next = await nextUni(fx.uni);
		if (!next) throw new Error("missing group after narrowed range");
		expect((await readGroup(next)).sequence).toBe(1);
	} finally {
		open.finish();
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: widening with a fill keeps the open live group", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1);
	const open = track.appendGroup();
	open.writeFrame({ payload: new TextEncoder().encode("1.0"), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({ requestId: 0n, trackNamespace: Path.from("test"), trackName: "video", subscriberPriority: 0 }),
	);
	try {
		const live = await nextUni(fx.uni);
		if (!live) throw new Error("missing initial open group");
		const reading = readGroup(live);
		const parameters = new Parameters();
		parameters.subscriptionFilter = { kind: "absolute", startGroup: 1n, startObject: 0n };
		parameters.fillParameters = Filter.encodeFill(
			{ filter: { kind: "relative", groups: 1n }, groupOrder: "ascending", rangeFilters: false },
			fx.version,
		);
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing update fill");
		expect((await readFill(fill, "ascending")).objects).toEqual([{ group: 1, id: 0, payload: "1.0" }]);
		open.writeFrame({ payload: new TextEncoder().encode("1.1"), timestamp: Timestamp.now() });
		open.finish();
		// The live stream still owns the rest of its group, so nothing else repeats it.
		expect((await reading).objects).toEqual([
			{ id: 0, payload: "1.0" },
			{ id: 1, payload: "1.1" },
		]);
		expect(await nextUni(fx.uni)).toBeUndefined();
	} finally {
		open.finish();
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: a fill-only update short of the edge keeps the live join's floor", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1);
	const open = track.appendGroup();
	open.writeFrame({ payload: new TextEncoder().encode("1.0"), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({ requestId: 0n, trackNamespace: Path.from("test"), trackName: "video", subscriberPriority: 0 }),
	);
	try {
		const live = await nextUni(fx.uni);
		if (!live) throw new Error("missing initial open group");
		const reading = readGroup(live);
		const parameters = new Parameters();
		parameters.fillParameters = Filter.encodeFill(
			{
				filter: { kind: "absolute", startGroup: 0n, startObject: 0n, endGroup: 0n },
				groupOrder: "ascending",
				rangeFilters: false,
			},
			fx.version,
		);
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing update fill");
		expect((await readFill(fill, "ascending")).reset).toBeDefined();
		open.writeFrame({ payload: new TextEncoder().encode("1.1"), timestamp: Timestamp.now() });
		open.finish();
		expect((await reading).objects).toEqual([
			{ id: 0, payload: "1.0" },
			{ id: 1, payload: "1.1" },
		]);
		// The refused fill never changes the live cursor.
		expect(await nextUni(fx.uni)).toBeUndefined();
	} finally {
		open.finish();
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test("draft-22: a fill-only update keeps the subscription's floor", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1);
	const open = track.appendGroup();
	open.writeFrame({ payload: new TextEncoder().encode("1.0"), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({ requestId: 0n, trackNamespace: Path.from("test"), trackName: "video", subscriberPriority: 0 }),
	);
	try {
		const live = await nextUni(fx.uni);
		if (!live) throw new Error("missing initial open group");
		const reading = readGroup(live);
		const parameters = new Parameters();
		parameters.fillParameters = Filter.encodeFill(
			{ filter: { kind: "relative", groups: 1n }, groupOrder: "ascending", rangeFilters: false },
			fx.version,
		);
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("missing update fill");
		expect((await readFill(fill, "ascending")).objects).toEqual([{ group: 1, id: 0, payload: "1.0" }]);
		open.writeFrame({ payload: new TextEncoder().encode("1.1"), timestamp: Timestamp.now() });
		open.finish();
		expect((await reading).objects).toEqual([
			{ id: 0, payload: "1.0" },
			{ id: 1, payload: "1.1" },
		]);
		// The live join's floor stays at group 1, so group 0 is never offered.
		expect(await nextUni(fx.uni)).toBeUndefined();
	} finally {
		open.finish();
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

test.each([
	[4n, 0n],
	[1n, 1n],
])("draft-22: widening with a fill resumes a group a narrowing to %p.%p gave up", async (group, object) => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	writeGroup(track, 1);
	const open = track.appendGroup();
	open.writeFrame({ payload: new TextEncoder().encode("1.0"), timestamp: Timestamp.now() });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({ requestId: 0n, trackNamespace: Path.from("test"), trackName: "video", subscriberPriority: 0 }),
	);
	try {
		const live = await nextUni(fx.uni);
		if (!live) throw new Error("missing initial open group");
		const reading = readGroup(live).then(() => undefined, error);
		for (const [requestId, startGroup, startObject] of [
			[2n, group, object],
			[4n, 1n, 0n],
		] as const) {
			const parameters = new Parameters();
			parameters.subscriptionFilter = { kind: "absolute", startGroup, startObject };
			if (requestId === 4n)
				parameters.fillParameters = Filter.encodeFill(
					{ filter: { kind: "relative", groups: 1n }, groupOrder: "ascending", rangeFilters: false },
					fx.version,
				);
			await client.writer.u53(SubscribeUpdate.id);
			await new SubscribeUpdate({ requestId, parameters }).encode(client.writer, fx.version);
			expect(await client.reader.u53()).toBe(RequestOk.id);
			await RequestOk.decode(client.reader, fx.version);
		}
		expect(await reading).toMatchObject({ code: StreamCode.Cancel });
		open.writeFrame({ payload: new TextEncoder().encode("1.1"), timestamp: Timestamp.now() });
		open.finish();
		expect((await readServed(fx.uni, 2)).sort()).toEqual(["1.0", "1.1"]);
	} finally {
		open.finish();
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});

/** The payloads of the next `count` streams, each a fill or a group in whatever order they open. */
async function readServed(
	uni: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>,
	count: number,
): Promise<string[]> {
	const served: string[] = [];
	for (let i = 0; i < count; i++) {
		const stream = await nextUni(uni);
		if (!stream) throw new Error("missing stream after widening");
		const reader = new Reader(stream, undefined, V22);
		if ((await reader.peekU62()) === BigInt(FetchHeader.type))
			served.push(...(await readFill(reader, "ascending")).objects.map(({ payload }) => payload));
		else served.push(...(await readGroup(reader)).objects.map(({ payload }) => payload));
	}
	return served;
}

test("subscriber max age and REQUEST_UPDATE preserve omitted preferences", async () => {
	const fx = fixture(Version.DRAFT_18);
	const track = fx.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 0n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 128,
			maxAge: 250,
		}),
	);
	try {
		expect(track.subscription.peek()?.maxDelay).toBe(Milli(250));
		const parameters = new Parameters();
		parameters.maxAge = 900;
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 2n, parameters }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(track.subscription.peek()?.maxDelay).toBe(Milli(900));
		await client.writer.u53(SubscribeUpdate.id);
		await new SubscribeUpdate({ requestId: 4n }).encode(client.writer, fx.version);
		expect(await client.reader.u53()).toBe(RequestOk.id);
		await RequestOk.decode(client.reader, fx.version);
		expect(track.subscription.peek()?.maxDelay).toBe(Milli(900));
	} finally {
		client.close();
		track.finish();
		track.close();
		fx.close();
	}
});
