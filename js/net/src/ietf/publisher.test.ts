import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { createMockTransportPair } from "../mock.ts";
import { type Origin, OriginSchema } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Timestamp } from "../time.ts";
import type { Producer as TrackProducer } from "../track.ts";
import { NativeSession, type Session } from "./adapter.ts";
import type * as Cluster from "./cluster.ts";
import { FetchHeader } from "./fetch.ts";
import { Group as GroupMessage } from "./object.ts";
import { PublishDone } from "./publish.ts";
import { PublishNamespace } from "./publish_namespace.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { SubscribeNamespace } from "./subscribe_namespace.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;

/** How long to wait for a stream before calling it absent, which is how a regression reports. */
const STREAM_WAIT = 1000;

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
	await new RequestOk({ requestId: undefined }).encode(stream.writer, VERSION);
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
			requestId: undefined,
			errorCode: 403,
			reasonPhrase: "no",
			retryInterval,
		}).encode(stream.writer, VERSION);
	} catch {
		// The publisher reset the request out from under us.
	}
}

function publisher(transport: WebTransport, cluster?: Cluster.Hops): Publisher {
	const session = new NativeSession(transport, VERSION, true);
	return new Publisher({ quic: transport, session, requiresSolicitation: false, cluster });
}

/**
 * Every advertisement waits a round trip for the peer's reply. A broadcast published in
 * that window has to survive it: the loop is not watching the signal while it waits, so
 * a listener registered afterwards would sleep through the notification and leave the
 * namespace unadvertised until something unrelated changed.
 */
test("a broadcast published mid-advertisement is still announced", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);

	const first = new BroadcastProducer();
	pub.publish(Path.from("first"), first);

	void pub.runPublishNamespaces();

	// Take the first advertisement but withhold the reply, parking the loop.
	const one = await nextStream(pair.client);
	if (!one) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(one)).toBe(Path.from("first"));

	// Publish while the loop is parked on that reply.
	const second = new BroadcastProducer();
	pub.publish(Path.from("second"), second);
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	await acceptPublishNamespace(one);

	const two = await nextStream(pair.client);
	if (!two) throw new Error("the broadcast published mid-advertisement was never announced");
	expect(await readPublishNamespace(two)).toBe(Path.from("second"));
	await acceptPublishNamespace(two);

	pub.close();
});

/**
 * A peer may decline an advertisement and stay connected. Recording it as advertised
 * anyway would strand the namespace: nothing re-adds it to the diff, so it would never
 * be offered again for the life of the session.
 */
test("a declined advertisement is retried on the next change", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);

	const first = new BroadcastProducer();
	pub.publish(Path.from("first"), first);

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("no PUBLISH_NAMESPACE for the first broadcast");
	expect(await readPublishNamespace(declined)).toBe(Path.from("first"));
	await declinePublishNamespace(declined);

	// Any later change re-runs the diff, which is where the refused namespace has to
	// reappear rather than being remembered as up.
	const second = new BroadcastProducer();
	pub.publish(Path.from("second"), second);

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("second"));
	expect(seen).toContain(Path.from("first"));

	pub.close();
});

/**
 * A peer out of stream credit rejects the open. That has to cost the namespace a turn,
 * not the session its discovery: the announce loop is never restarted, so unwinding it
 * would lose every future publish too.
 */
test("a failed stream open does not kill the announce loop", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, VERSION, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	const pub = new Publisher({ quic: pair.server, session, requiresSolicitation: false });
	pub.publish(Path.from("first"), new BroadcastProducer());

	void pub.runPublishNamespaces();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));

	// The refused open cost "first" its turn; the next change has to bring it back along
	// with the newcomer.
	pub.publish(Path.from("second"), new BroadcastProducer());

	const seen = new Set<Path.Valid>();
	for (let i = 0; i < 2; i++) {
		const stream = await nextStream(pair.client);
		if (!stream) break;
		seen.add(await readPublishNamespace(stream));
		await acceptPublishNamespace(stream);
	}

	expect(seen).toContain(Path.from("first"));
	expect(seen).toContain(Path.from("second"));

	pub.close();
});

/**
 * Capacity coming back raises no signal of its own: no broadcast is published, closed, or
 * changed. The loop has to come back and ask again on its own, or a namespace refused
 * once stays undiscoverable for the session.
 */
test("a namespace refused once is retried without anything else changing", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, VERSION, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	const pub = new Publisher({ quic: pair.server, session, requiresSolicitation: false });
	pub.publish(Path.from("lonely"), new BroadcastProducer());

	void pub.runPublishNamespaces();

	// Nothing else happens: no second publish, no close. Only the retry can save it.
	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the refused namespace was never retried");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await acceptPublishNamespace(stream);

	pub.close();
});

/**
 * The solicited legacy path advertises with PUBLISH_NAMESPACE requests too, so a declined
 * one needs the same retry the unsolicited loop has. Without it, a namespace refused once
 * stays undiscoverable for the life of the subscription, since the peer starting to
 * answer raises no signal the loop is watching.
 */
test("a solicited legacy advertisement refused once is retried", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const inner = new NativeSession(pair.server, Version.DRAFT_15, true);

	let failures = 1;
	const session: Session = {
		version: inner.version,
		acceptBi: () => inner.acceptBi(),
		nextRequestId: () => inner.nextRequestId(),
		close: () => inner.close(),
		openBi: () => {
			if (failures-- > 0) throw new Error("no stream credit");
			return inner.openBi();
		},
	};

	// The peer declared that advertisements to it must be solicited, so this is the loop
	// that answers its SUBSCRIBE_NAMESPACE.
	const pub = new Publisher({ quic: pair.server, session, requiresSolicitation: true });
	pub.publish(Path.from("lonely"), new BroadcastProducer());

	const subscription = await Stream.open(pair.client, { version: Version.DRAFT_15 });
	const accepted = await Stream.accept(pair.server, Version.DRAFT_15);
	if (!accepted) throw new Error("the subscription stream was never accepted");
	void pub.runSubscribeNamespace(new SubscribeNamespace({ requestId: 0n, namespace: Path.empty() }), accepted);

	// Nothing else happens: no second publish, no close. Only the retry can save it.
	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the refused namespace was never retried");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await acceptPublishNamespace(stream);

	subscription.close();
	pub.close();
});

/**
 * A peer that refuses an advertisement with a retry interval of 0 is asking not to be
 * offered it again. Coming back anyway turns a permanent refusal (unauthorized,
 * uninterested) into a request every few seconds for the life of the session.
 */
test("a refusal that forbids retrying is not retried", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);
	pub.publish(Path.from("lonely"), new BroadcastProducer());

	void pub.runPublishNamespaces();

	const stream = await nextStream(pair.client);
	if (!stream) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(stream)).toBe(Path.from("lonely"));
	await declinePublishNamespace(stream, 0n);

	// Well past the retry the loop would otherwise take.
	expect(await nextStream(pair.client)).toBeUndefined();

	pub.close();
});

/**
 * A refusal belongs to the namespace, not the path forever. Unannouncing takes it with
 * it, so a fresh broadcast at the same path is offered again; keeping it would strand
 * that path for the life of the session with no timer able to recover it. Rust gets this
 * by rebuilding the watched entry on re-announce.
 */
test("re-announcing a path clears a refusal that forbade retrying", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);

	const first = new BroadcastProducer();
	pub.publish(Path.from("recycled"), first);

	void pub.runPublishNamespaces();

	const declined = await nextStream(pair.client);
	if (!declined) throw new Error("the namespace was never advertised");
	expect(await readPublishNamespace(declined)).toBe(Path.from("recycled"));
	await declinePublishNamespace(declined, 0n);

	// The broadcast goes away, taking the refusal with it, and a new one takes its place.
	first.close();
	await new Promise((resolve) => setTimeout(resolve, SETTLE));
	pub.publish(Path.from("recycled"), new BroadcastProducer());

	const retried = await nextStream(pair.client);
	if (!retried) throw new Error("a re-announced path was never offered again");
	expect(await readPublishNamespace(retried)).toBe(Path.from("recycled"));
	await acceptPublishNamespace(retried);

	pub.close();
});

/**
 * A peer that declared a Hop ID gets one on every advertisement: it is what lets the peer
 * tell that an advertisement it hears back came from us. A peer that declared nothing has
 * not read ours either, so sending it the parameters would be a protocol violation.
 */
test("an advertisement carries our hop id once the peer declared one", async () => {
	const self: Origin = OriginSchema.parse(7n);

	for (const peer of [OriginSchema.parse(9n), undefined]) {
		const pair = createMockTransportPair(ALPN.DRAFT_19);
		const pub = publisher(pair.server, { self, peer });
		pub.publish(Path.from("mine"), new BroadcastProducer());
		void pub.runPublishNamespaces();

		const stream = await nextStream(pair.client);
		if (!stream) throw new Error("no PUBLISH_NAMESPACE for the broadcast");
		expect(await stream.reader.u53()).toBe(PublishNamespace.id);

		if (peer === undefined) {
			// Nothing negotiated, so the parameters are absent: reading the message as a
			// negotiated one finds no HOP_PATH and rejects.
			await expect(PublishNamespace.decode(stream.reader, VERSION, true)).rejects.toThrow();
		} else {
			// Our own Hop ID is the last entry, and we originate everything we advertise,
			// so it is the only one. The cost is 0: we are already producing the content.
			const msg = await PublishNamespace.decode(stream.reader, VERSION, true);
			expect(msg.trackNamespace).toBe(Path.from("mine"));
			expect(msg.cluster).toEqual({ hops: [self], cost: 0n });
			await acceptPublishNamespace(stream);
		}

		pub.close();
	}
});

test("a same-tick republish survives its predecessor closing", async () => {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const pub = publisher(pair.server);
	const path = Path.from("test");

	const first = new BroadcastProducer();
	pub.publish(path, first);
	first.close();

	const second = new BroadcastProducer();
	second.createTrack("video");
	pub.publish(path, second);

	await first.closed;

	const client = await Stream.open(pair.client);
	const server = await nextStream(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ requestId: 0n, trackNamespace: path, trackName: "video", subscriberPriority: 0 });
	void pub.runSubscribe(msg, server);

	try {
		expect(await client.reader.u53()).toBe(SubscribeOk.id);
	} finally {
		pub.close();
		client.close();
	}
});

test("subscription completion sends PUBLISH_DONE on every supported draft", async () => {
	const versions = [
		Version.DRAFT_14,
		Version.DRAFT_15,
		Version.DRAFT_16,
		Version.DRAFT_17,
		Version.DRAFT_18,
		Version.DRAFT_19,
	] as const;

	for (const version of versions) {
		for (const abort of [undefined, new Error("failed")]) {
			const pair = createMockTransportPair(ALPN.DRAFT_19);
			const session = new NativeSession(pair.server, version, true);
			const pub = new Publisher({ quic: pair.server, session, requiresSolicitation: false });
			const broadcast = new BroadcastProducer();
			const track = broadcast.createTrack("video");
			const path = Path.from("test");
			pub.publish(path, broadcast);

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
			track.close(abort);

			expect(await client.reader.u53()).toBe(PublishDone.id);
			const done = await PublishDone.decode(client.reader, version);
			expect(done.requestId).toBe(version <= Version.DRAFT_16 ? requestId : undefined);
			expect(done.statusCode).toBe(abort ? 0x0 : 0x2);

			await running;
			pub.close();
			client.close();
		}
	}
});

/** Draft-20 is the only version whose Location Filters and fills the publisher acts on. */
const V20 = Version.DRAFT_20;

/** A group stream the publisher opened, decoded down to its objects. */
interface ServedGroup {
	/** The group's sequence number. */
	sequence: number;
	/** Whether the header claimed the stream starts at the group's first object. */
	firstObject: boolean;
	/** Each object's absolute id (reconstructed from its delta) and payload. */
	objects: { id: number; payload: string }[];
}

/** A fill's fetch stream, decoded down to its objects. */
interface ServedFill {
	/** The request id the FETCH_HEADER named, when it survived a reset. */
	requestId?: bigint;
	/** Each object's group, absolute id, and payload. */
	objects: { group: number; id: number; payload: string }[];
	/** Whether the publisher reset the stream instead of finishing it. */
	reset: boolean;
}

/**
 * A publisher serving one broadcast over draft-20, with the subscribe stream already open.
 *
 * The uni reader is taken up front: a group stream opened before the test asks for one still
 * queues, but taking the reader late races the publisher rather than the test.
 */
function fixture(): {
	pair: ReturnType<typeof createMockTransportPair>;
	pub: Publisher;
	broadcast: BroadcastProducer;
	uni: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>;
	close: () => void;
} {
	const pair = createMockTransportPair(ALPN.DRAFT_20);
	const session = new NativeSession(pair.server, V20, true);
	const pub = new Publisher({ quic: pair.server, session, requiresSolicitation: false });
	const broadcast = new BroadcastProducer();
	pub.publish(Path.from("test"), broadcast);
	const uni = pair.client.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;

	return {
		pair,
		pub,
		broadcast,
		uni,
		close: () => {
			uni.releaseLock();
			pub.close();
		},
	};
}

/** Write `frames` numbered payloads into a new closed group. */
function writeGroup(track: TrackProducer, frames: number): void {
	const group = track.appendGroup();
	for (let i = 0; i < frames; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`${group.sequence}.${i}`), timestamp: Timestamp.now() });
	}
	group.close();
}

/** Send `msg` on a fresh subscribe stream and read the publisher's SUBSCRIBE_OK. */
async function runSubscribe(
	fx: ReturnType<typeof fixture>,
	msg: Subscribe,
): Promise<{ client: Stream; ok: SubscribeOk }> {
	const client = await Stream.open(fx.pair.client, { version: V20 });
	const server = await Stream.accept(fx.pair.server, V20);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	void fx.pub.runSubscribe(msg, server);

	expect(await client.reader.u53()).toBe(SubscribeOk.id);
	return { client, ok: await SubscribeOk.decode(client.reader, V20) };
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
async function readGroup(stream: ReadableStream<Uint8Array>): Promise<ServedGroup> {
	const reader = new Reader(stream, undefined, V20);
	const header = await GroupMessage.decode(reader, V20);

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

	return { sequence: header.groupId, firstObject: header.flags.firstObject, objects };
}

/**
 * Read a fill's fetch stream to its end, reporting a reset rather than throwing.
 *
 * A reset discards data the peer has not acknowledged, so a refused fill may lose its
 * FETCH_HEADER along with the rest: the request id is only reported when it arrived.
 */
async function readFill(stream: ReadableStream<Uint8Array>): Promise<ServedFill> {
	const reader = new Reader(stream, undefined, V20);
	const objects: { group: number; id: number; payload: string }[] = [];
	let requestId: bigint | undefined;
	let group = 0;
	let id = 0;
	try {
		expect(await reader.u53()).toBe(FetchHeader.type);
		requestId = (await FetchHeader.decode(reader, V20)).requestId;

		while (!(await reader.done())) {
			const flags = await reader.u53();
			if (flags & 0x08) group = await reader.u53();
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
	} catch {
		return { requestId, objects, reset: true };
	}

	return { requestId, objects, reset: false };
}

/**
 * An absolute filter names the objects it wants, so the boundary groups are trimmed to it
 * and the groups outside it are never opened. The first object written carries its absolute
 * id, or the subscriber would read a silently renumbered group.
 */
test("draft-20: an absolute filter trims the range it serves", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video");
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
		const first = await nextUni(fx.uni);
		if (!first) throw new Error("the filter's start group was never served");
		expect(await readGroup(first)).toEqual({
			sequence: 1,
			// The head was trimmed, so the stream does not start at the group's first object.
			firstObject: false,
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
			objects: [{ id: 0, payload: "2.0" }],
		});

		// Groups 0 and 3 are outside the range, so nothing more is opened.
		expect(await nextUni(fx.uni)).toBeUndefined();
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
test("draft-20: a fill serves the current group's head on a fetch stream", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video");

	const group = track.appendGroup();
	for (let i = 0; i < 2; i++) {
		group.writeFrame({ payload: new TextEncoder().encode(`0.${i}`), timestamp: Timestamp.now() });
	}

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
		// A fill-requesting subscriber sizes its backfill against this.
		expect(ok.largest).toEqual({ groupId: 0n, objectId: 1n });

		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("no fetch stream for the requested fill");
		expect(await readFill(fill)).toEqual({
			requestId: 7n,
			objects: [
				{ group: 0, id: 0, payload: "0.0" },
				{ group: 0, id: 1, payload: "0.1" },
			],
			reset: false,
		});

		// Everything past the snapshot belongs to the subscription, not the fill.
		group.writeFrame({ payload: new TextEncoder().encode("0.2"), timestamp: Timestamp.now() });
		group.close();

		const live = await nextUni(fx.uni);
		if (!live) throw new Error("the subscription never served the live tail");
		expect(await readGroup(live)).toEqual({
			sequence: 0,
			firstObject: false,
			objects: [{ id: 2, payload: "0.2" }],
		});
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * Multi-group fetch serialization depends on a negotiated group order we do not implement.
 * A fill is a promise once requested, so the stream still opens and is reset right after the
 * FETCH_HEADER, which is the draft's fill-failure signal.
 */
test("draft-20: a fill spanning several groups resets its stream", async () => {
	const fx = fixture();
	const track = fx.broadcast.createTrack("video");
	for (let i = 0; i < 3; i++) writeGroup(track, 2);

	const { client } = await runSubscribe(
		fx,
		new Subscribe({
			requestId: 7n,
			trackNamespace: Path.from("test"),
			trackName: "video",
			subscriberPriority: 0,
			filter: { kind: "nextObject" },
			fill: { filter: { kind: "relative", groups: 2n }, rangeFilters: false },
		}),
	);

	try {
		const fill = await nextUni(fx.uni);
		if (!fill) throw new Error("a refused fill still owes the subscriber a reset stream");
		const served = await readFill(fill);
		expect(served.reset).toBe(true);
		expect(served.objects).toEqual([]);
	} finally {
		fx.close();
		client.close();
	}
});

/** A fill against a track with nothing published has an empty range: no stream is owed. */
test("draft-20: an empty track opens no fill stream", async () => {
	const fx = fixture();
	fx.broadcast.createTrack("video");

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
		expect(await nextUni(fx.uni)).toBeUndefined();
	} finally {
		fx.close();
		client.close();
	}
});

/**
 * INCLUDE_PROPERTIES=0 opts the response out of Track Properties, which also opts the track
 * out of timestamps: with no declared Timescale there are no units to read one in.
 */
test("draft-20: an opt-out peer gets no track properties", async () => {
	for (const propertiesWanted of [true, false]) {
		const fx = fixture();
		fx.broadcast.createTrack("video");

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

		fx.close();
		client.close();
	}
});
