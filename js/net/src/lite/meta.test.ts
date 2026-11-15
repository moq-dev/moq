// Metadata-only subscriptions (lite-07): the wire fields, metadata frames in the group model, how a
// track's readers are gated by mode, and the mode across sessions and relays.

import { afterEach, expect, mock, test } from "bun:test";
import { accept, connect, type Session } from "../connection/index.ts";
import { ProtocolViolation, Unsupported } from "../error.ts";
import { Producer as GroupProducer } from "../group.ts";
import { randomHop } from "../hop.ts";
import * as Ietf from "../ietf/index.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair, type MockTransport, waitFor } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { concat, Reader, Stream, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { Producer as TrackProducer, type Subscriber as TrackSubscriber } from "../track.ts";
import { wireOf } from "../wire.ts";
import { frameDecoder, Group as GroupMessage, readFrames } from "./group.ts";
import { frameHeader, Publisher } from "./publisher.ts";
import { Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");
const path = Path.from("room");

afterEach(() => mock.restore());

async function encode(
	version: Version,
	message: { encode(writer: Writer, version: Version): Promise<void> },
): Promise<Uint8Array> {
	const written: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void written.push(new Uint8Array(chunk)) }),
		version,
	);
	await message.encode(writer, version);
	writer.close();
	await writer.closed;
	return concat(written);
}

function reader(bytes: Uint8Array, version: Version): Reader {
	return new Reader(undefined, bytes, version);
}

// --- Codec ---

const subscribe = (meta: boolean) => new Subscribe({ id: 1n, broadcast: path, track: "video", priority: 0, meta });

test("SUBSCRIBE carries Meta last on lite-07, and an older wire refuses to ask for it", async () => {
	const bytes = await encode(Version.DRAFT_07, subscribe(true));
	// Warm, then Meta.
	expect([...bytes.slice(-2)]).toEqual([0, 1]);
	expect((await Subscribe.decode(reader(bytes, Version.DRAFT_07), Version.DRAFT_07)).meta).toBe(true);

	// Any other byte is malformed.
	const bad = bytes.slice();
	bad[bad.length - 1] = 2;
	await expect(Subscribe.decode(reader(bad, Version.DRAFT_07), Version.DRAFT_07)).rejects.toThrow(ProtocolViolation);

	await expect(encode(Version.DRAFT_06, subscribe(true))).rejects.toThrow("not supported");
	const old = await encode(Version.DRAFT_06, subscribe(false));
	expect((await Subscribe.decode(reader(old, Version.DRAFT_06), Version.DRAFT_06)).meta).toBe(false);
});

test("SUBSCRIBE_UPDATE carries Meta and the payload position, its group plus one", async () => {
	const update = (meta: boolean, payload?: { group: bigint; frame: number }) =>
		new SubscribeUpdate({ priority: 0, ranges: [], update: 1n, meta, payload });

	const promote = await encode(Version.DRAFT_07, update(false, { group: 0n, frame: 5 }));
	expect([...promote.slice(-3)]).toEqual([0, 1, 5]);
	const got = await SubscribeUpdate.decode(reader(promote, Version.DRAFT_07), Version.DRAFT_07);
	expect([got.meta, got.payload]).toEqual([false, { group: 0n, frame: 5 }]);

	const demote = await encode(Version.DRAFT_07, update(true));
	expect([...demote.slice(-3)]).toEqual([1, 0, 0]);
	const back = await SubscribeUpdate.decode(reader(demote, Version.DRAFT_07), Version.DRAFT_07);
	expect([back.meta, back.payload]).toEqual([true, undefined]);

	const bad = demote.slice();
	bad[bad.length - 3] = 7;
	await expect(SubscribeUpdate.decode(reader(bad, Version.DRAFT_07), Version.DRAFT_07)).rejects.toThrow(
		ProtocolViolation,
	);
	await expect(encode(Version.DRAFT_06, update(true))).rejects.toThrow("not supported");
});

test("GROUP marks each stream's mode on lite-07 only", async () => {
	const group = (meta: boolean) => new GroupMessage({ subscribe: 1n, sequence: 7n, meta });
	const bytes = await encode(Version.DRAFT_07, group(true));
	expect(bytes.at(-1)).toBe(1);
	expect((await GroupMessage.decode(reader(bytes, Version.DRAFT_07), Version.DRAFT_07)).meta).toBe(true);

	const bad = bytes.slice();
	bad[bad.length - 1] = 2;
	await expect(GroupMessage.decode(reader(bad, Version.DRAFT_07), Version.DRAFT_07)).rejects.toThrow(
		ProtocolViolation,
	);
	await expect(encode(Version.DRAFT_06, group(true))).rejects.toThrow("not supported");
	const old = await encode(Version.DRAFT_06, group(false));
	expect((await GroupMessage.decode(reader(old, Version.DRAFT_06), Version.DRAFT_06)).meta).toBe(false);
});

test("a metadata FRAME is its payload length and timestamp, and nothing else", async () => {
	// A 1000-byte frame at 40ms, then an empty one 20ms later.
	const first = frameHeader(Version.DRAFT_07, 40n, undefined, 1000, true);
	const second = frameHeader(Version.DRAFT_07, 20n, undefined, 0, true);
	const decode = frameDecoder({ scale: 1000, lengthFirst: true, compress: true, meta: true });
	const r = reader(concat([first, second]), Version.DRAFT_07);
	const a = r.tryDecode(decode);
	const b = r.tryDecode(decode);
	expect([a?.meta, a?.size, a?.payload, a?.timestamp?.value]).toEqual([true, 1000, undefined, 40]);
	expect([b?.meta, b?.size, b?.timestamp?.value]).toEqual([true, 0, 60]);
	// Neither an inflated size nor a payload followed: the stream is drained.
	expect(r.tryDecode(decode)).toBeUndefined();
});

// --- Group model ---

test("a metadata group reads headers, and a payload read of it fails loudly", async () => {
	const producer = new GroupProducer(3n);
	hooks.metaGroup(producer);
	hooks.writeGroupHeader(producer, { size: 1000, timestamp: Timestamp.fromMillis(40) });
	hooks.writeGroupHeader(producer, { size: 0, timestamp: Timestamp.fromMillis(60) });
	producer.finish();
	expect(() => producer.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.fromMillis(80) })).toThrow();

	const group = producer.consume();
	expect(group.meta).toBe(true);
	expect(() => group.tryReadFrame()).toThrow("metadata group carries no payload");
	await expect(group.readFrame()).rejects.toThrow("metadata group carries no payload");

	const big = await group.nextFrame();
	expect([big?.sequence, big?.size, big?.timestamp.asMillis()]).toEqual([0, 1000, 40]);
	await expect(big?.readChunk()).rejects.toThrow("no payload");
	// A metadata frame of size 0 is still no empty payload.
	const empty = await group.nextFrame();
	expect(empty?.size).toBe(0);
	await expect(empty?.readAll()).rejects.toThrow("no payload");
	expect(await group.nextFrame()).toBeUndefined();
});

test("readInfo reads a metadata group's sizes and timestamps", async () => {
	const producer = new GroupProducer(3n);
	hooks.metaGroup(producer);
	hooks.writeGroupHeader(producer, { size: 1000, timestamp: Timestamp.fromMillis(40) });
	producer.finish();

	const group = producer.consume();
	const info = await group.readInfo();
	expect([info?.size, info?.timestamp.asMillis()]).toEqual([1000, 40]);
	expect(await group.readInfo()).toBeUndefined();
});

test("a metadata view of a payload group reads each header as soon as the frame exists", async () => {
	const producer = new GroupProducer(0n);
	const payload = producer.consume();
	const meta = producer.mirror(true);
	// Written in pieces: the header exists before any byte.
	const frame = producer.createFrame({ size: 4, timestamp: Timestamp.fromMillis(10) });
	const header = await meta.nextFrame();
	expect([meta.meta, header?.size]).toEqual([true, 4]);
	await expect(header?.readChunk()).rejects.toThrow("no payload");

	frame.write(new Uint8Array([1, 2, 3, 4]));
	frame.finish();
	// A real empty frame reads as an empty payload on the payload view.
	producer.writeFrame({ payload: new Uint8Array(0), timestamp: Timestamp.fromMillis(20) });
	producer.finish();
	expect(payload.meta).toBe(false);
	expect((await payload.readFrame())?.payload).toEqual(new Uint8Array([1, 2, 3, 4]));
	expect((await payload.readFrame())?.payload).toEqual(new Uint8Array(0));
	expect((await meta.nextFrame())?.size).toBe(0);
});

test("a metadata Group Stream writes headers into a metadata copy, and a continuation skips the held ones", async () => {
	const bytes = concat([
		frameHeader(Version.DRAFT_07, 5n, undefined, 300, true),
		frameHeader(Version.DRAFT_07, 5n, undefined, 2, true),
	]);
	const producer = new GroupProducer(0n);
	hooks.metaGroup(producer);
	const format = { scale: 1000, lengthFirst: true, meta: true };
	await readFrames(reader(bytes, Version.DRAFT_07), producer, format);
	expect(hooks.groupFrameCount(producer)).toBe(2);
	// A continuation from frame 1 re-sends the second header and adds a third.
	const more = concat([
		frameHeader(Version.DRAFT_07, 10n, undefined, 2, true),
		frameHeader(Version.DRAFT_07, 5n, undefined, 9, true),
	]);
	await readFrames(reader(more, Version.DRAFT_07), producer, format, { index: 1 });
	producer.finish();
	const group = producer.consume();
	const sizes: number[] = [];
	for (let frame = await group.nextFrame(); frame; frame = await group.nextFrame()) sizes.push(frame.size);
	expect(sizes).toEqual([300, 2, 9]);
});

// --- Track model ---

function writeGroup(track: TrackProducer, sequence: bigint, sizes: number[]): GroupProducer {
	const group = track.createGroup(sequence);
	for (const [index, size] of sizes.entries())
		group.writeFrame({ payload: new Uint8Array(size).fill(index + 1), timestamp: Timestamp.fromMillis(index) });
	return group;
}

test("the aggregate is metadata only while every subscriber asks for it", async () => {
	const track = new TrackProducer("video");
	const a = track.subscribe({ meta: true });
	const b = track.subscribe({ meta: true });
	expect(track.subscription.peek()?.meta).toBe(true);
	const c = track.subscribe({});
	expect(track.subscription.peek()?.meta).toBe(false);
	c.close();
	await Promise.resolve();
	expect(track.subscription.peek()?.meta).toBe(true);
	b.update({ meta: false });
	await Promise.resolve();
	expect(track.subscription.peek()?.meta).toBe(false);
	a.close();
	b.close();
	expect(() => track.subscribe({ meta: "yes" as unknown as boolean })).toThrow(TypeError);
});

test("a metadata reader reads a payload copy's headers, and a payload reader its bytes", async () => {
	const track = new TrackProducer("video");
	track.accept({ timescale: Timescale.MILLI, maxAge: null });
	const meta = track.subscribe({ meta: true, delay: { max: null } });
	const payload = track.subscribe({ delay: { max: null } });
	writeGroup(track, 0n, [5, 6]).finish();

	const header = await meta.recvGroup();
	expect(header?.meta).toBe(true);
	expect((await header?.nextFrame())?.size).toBe(5);
	const body = await payload.recvGroup();
	expect(body?.meta).toBe(false);
	expect((await body?.readFrame())?.payload.byteLength).toBe(5);
	meta.close();
	payload.close();
});

// A received copy of `track`'s group `sequence`, as a session writes one.
function receive(track: TrackProducer, sequence: bigint, meta: boolean, sizes: number[]): GroupProducer {
	const group = hooks.receiveGroup(track, { sequence, start: 0, meta });
	for (const [index, size] of sizes.entries()) {
		const timestamp = Timestamp.fromMillis(index);
		if (meta) hooks.writeGroupHeader(group, { size, timestamp });
		else group.writeFrame({ payload: new Uint8Array(size), timestamp });
	}
	return group;
}

test("a payload reader never gets a metadata copy, and the payload replacing it reaches every reader once", async () => {
	const track = new TrackProducer("video");
	track.accept({ timescale: Timescale.MILLI, maxAge: null });
	const meta = track.subscribe({ meta: true, delay: { max: null } });
	const payload = track.subscribe({ delay: { max: null } });

	receive(track, 0n, true, [5]).finish();
	const header = await meta.recvGroup();
	expect([header?.sequence, header?.meta]).toEqual([0n, true]);
	// For the payload reader the group has yet to arrive.
	expect(payload.tryRecvGroup()).toBeUndefined();

	receive(track, 0n, false, [5]).finish();
	const body = await payload.recvGroup();
	expect([body?.sequence, body?.meta]).toEqual([0n, false]);
	expect((await body?.readFrame())?.payload.byteLength).toBe(5);
	// The metadata reader took group 0 already, so the payload copy is not handed out twice.
	expect(meta.tryRecvGroup()).toBeUndefined();

	// Metadata over a payload copy adds nothing: it is read to its end and discarded.
	const discarded = receive(track, 0n, true, [5]);
	discarded.finish();
	expect(meta.tryRecvGroup()).toBeUndefined();
	expect(payload.tryRecvGroup()).toBeUndefined();
	meta.close();
	payload.close();
});

test("promoting a reader hands out again the groups it read as metadata, from the payload position", async () => {
	const track = new TrackProducer("video");
	track.accept({ timescale: Timescale.MILLI, maxAge: null });
	const reader = track.subscribe({ meta: true, delay: { max: null } });
	for (const sequence of [0n, 1n, 2n]) writeGroup(track, sequence, [3]).finish();
	const read: bigint[] = [];
	for (let i = 0; i < 3; i++) {
		const group = await reader.recvGroup();
		expect(group?.meta).toBe(true);
		read.push(group?.sequence ?? -1n);
	}
	expect(read).toEqual([0n, 1n, 2n]);

	// From group 1: group 0 stays delivered, and 1 and 2 come again with their bytes.
	hooks.updateSubscriber(reader, { meta: false, delay: { max: null } }, { group: 1n, frame: 0 });
	const again: bigint[] = [];
	for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) {
		expect(group.meta).toBe(false);
		expect((await group.readFrame())?.payload.byteLength).toBe(3);
		again.push(group.sequence);
	}
	expect(again).toEqual([1n, 2n]);

	// A plain update with no position asks for every group read as metadata since.
	reader.update({ meta: true, delay: { max: null } });
	writeGroup(track, 3n, [3]).finish();
	expect((await reader.recvGroup())?.meta).toBe(true);
	reader.update({ meta: false, delay: { max: null } });
	expect(reader.tryRecvGroup()?.sequence).toBe(3n);
	expect(reader.tryRecvGroup()).toBeUndefined();
	reader.close();
});

test("an update re-reads the unread groups in the new mode, and leaves handed-out ones alone", async () => {
	const track = new TrackProducer("video");
	track.accept({ timescale: Timescale.MILLI, maxAge: null });
	const reader = track.subscribe({ delay: { max: null } });
	writeGroup(track, 0n, [3]).finish();
	writeGroup(track, 1n, [3]).finish();
	const held = await reader.recvGroup();
	// Demoted: the unread group 1 is read as metadata, while group 0 keeps its payload.
	reader.update({ meta: true, delay: { max: null } });
	const next = await reader.recvGroup();
	expect([held?.meta, next?.sequence, next?.meta]).toEqual([false, 1n, true]);
	expect((await held?.readFrame())?.payload.byteLength).toBe(3);
	reader.close();
});

// --- Sessions and relays ---

// Count the bytes `transport` writes on its unidirectional (group) streams.
function countGroupBytes(transport: MockTransport): { bytes: number } {
	const counter = { bytes: 0 };
	const open = transport.createUnidirectionalStream.bind(transport);
	transport.createUnidirectionalStream = async (options) => {
		const stream = await open(options);
		const getWriter = stream.getWriter.bind(stream);
		stream.getWriter = () => {
			const writer = getWriter();
			const write = writer.write.bind(writer);
			writer.write = (chunk) => {
				if (chunk) counter.bytes += chunk.byteLength;
				return write(chunk);
			};
			return writer;
		};
		return stream;
	};
	return counter;
}

/**
 * A publisher's session to a relay, and the relay's session to a viewer, both on `version`. The
 * relay serves the viewer from its received copy of the publisher's broadcast, as a JS relay does.
 */
async function chain(version: Version = Version.DRAFT_07) {
	const alpn = version === Version.DRAFT_07 ? ALPN_07_WIP : ALPN_06;
	const upstream = createMockTransportPair(alpn);
	const downstream = createMockTransportPair(alpn);
	const sent = { up: countGroupBytes(upstream.client), down: countGroupBytes(downstream.server) };
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const sessions: Session[] = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url, version }),
		accept({ transport: downstream.server, url, version, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);
	const [, ingest, , viewer] = sessions;
	const claim = relay.claim(path);
	const serving = (async () => {
		for await (const request of claim.requested()) request.serve(wireOf(ingest).consume(request.path));
	})();
	const broadcast = source.route(path).broadcast;
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	const remote = wireOf(viewer).consume(path);
	return {
		track,
		sent,
		subscribe: (meta: boolean): TrackSubscriber => remote.track("video").subscribe({ meta, delay: { max: null } }),
		/** Wait until the publisher's track is subscribed through the relay in `meta` mode. */
		async asked(meta: boolean) {
			await waitFor(track.subscription, (subscription) => subscription?.meta === meta);
		},
		async close() {
			remote.close();
			broadcast.close();
			claim.close();
			await serving;
			for (const session of sessions) session.abort();
			source.close();
			relay.close();
		},
	};
}

const BIG = 10_000;

test("metadata-only demand crosses two hops carrying no payload bytes", async () => {
	const hops = await chain();
	const reader = hops.subscribe(true);
	await hops.asked(true);
	for (const sequence of [0n, 1n]) writeGroup(hops.track, sequence, [BIG, BIG, 0]).finish();

	for (const sequence of [0n, 1n]) {
		const group = await reader.recvGroup();
		expect([group?.sequence, group?.meta]).toEqual([sequence, true]);
		const sizes: number[] = [];
		for (let frame = await group?.nextFrame(); frame; frame = await group?.nextFrame()) {
			sizes.push(frame.size);
			expect(frame.timestamp.asMillis()).toBe(frame.sequence);
		}
		expect(sizes).toEqual([BIG, BIG, 0]);
	}
	// Two groups of 20 KB of payload crossed each hop as a few bytes of headers.
	expect(hops.sent.up.bytes).toBeLessThan(100);
	expect(hops.sent.down.bytes).toBeLessThan(100);
	reader.close();
	await hops.close();
});

test("mixed demand pulls the payload, which the metadata reader reads as headers", async () => {
	const hops = await chain();
	const meta = hops.subscribe(true);
	const payload = hops.subscribe(false);
	await hops.asked(false);
	writeGroup(hops.track, 0n, [BIG]).finish();

	const header = await meta.recvGroup();
	expect(header?.meta).toBe(true);
	expect((await header?.nextFrame())?.size).toBe(BIG);
	const body = await payload.recvGroup();
	expect((await body?.readFrame())?.payload.byteLength).toBe(BIG);
	expect(hops.sent.up.bytes).toBeGreaterThan(BIG);
	meta.close();
	payload.close();
	await hops.close();
});

test("once the last payload reader leaves, the relay demotes upstream and keeps forwarding headers", async () => {
	const hops = await chain();
	const meta = hops.subscribe(true);
	const payload = hops.subscribe(false);
	await hops.asked(false);
	writeGroup(hops.track, 0n, [BIG]).finish();
	expect((await payload.recvGroup())?.sequence).toBe(0n);
	expect((await meta.recvGroup())?.sequence).toBe(0n);

	payload.close();
	await hops.asked(true);
	const before = { ...hops.sent.up };
	writeGroup(hops.track, 1n, [BIG]).finish();
	const header = await meta.recvGroup();
	expect([header?.sequence, header?.meta]).toEqual([1n, true]);
	expect((await header?.nextFrame())?.size).toBe(BIG);
	expect(hops.sent.up.bytes - before.bytes).toBeLessThan(50);

	// The last reader leaving releases the upstream subscription too.
	meta.close();
	await waitFor(hops.track.demand().wanted, (wanted) => wanted === undefined);
	await hops.close();
});

test("promotion during an open group serves its payload from frame 0 across both hops", async () => {
	const hops = await chain();
	const reader = hops.subscribe(true);
	await hops.asked(true);
	const group = writeGroup(hops.track, 0n, [BIG]);
	const header = await reader.recvGroup();
	expect([header?.sequence, header?.meta]).toEqual([0n, true]);
	expect((await header?.nextFrame())?.size).toBe(BIG);

	reader.update({ meta: false, delay: { max: null } });
	await hops.asked(false);
	// A frame written after the promotion; the frame before it was only ever sent as a header.
	group.writeFrame({ payload: new Uint8Array(5), timestamp: Timestamp.fromMillis(1) });
	group.finish();

	const body = await reader.recvGroup();
	expect([body?.sequence, body?.meta]).toEqual([0n, false]);
	expect((await body?.readFrame())?.payload.byteLength).toBe(BIG);
	expect((await body?.readFrame())?.payload.byteLength).toBe(5);
	expect(await body?.readFrame()).toBeUndefined();
	// The header stream keeps its own mode: its late frame arrives as a header.
	expect((await header?.nextFrame())?.size).toBe(5);

	// Demoted again, the next group is headers only.
	reader.update({ meta: true, delay: { max: null } });
	await hops.asked(true);
	const before = hops.sent.up.bytes;
	writeGroup(hops.track, 1n, [BIG]).finish();
	expect((await reader.recvGroup())?.meta).toBe(true);
	expect(hops.sent.up.bytes - before).toBeLessThan(50);
	reader.close();
	await hops.close();
});

test("an older lite version refuses metadata-only demand rather than fetch the payload", async () => {
	const hops = await chain(Version.DRAFT_06);
	const reader = hops.subscribe(true);
	expect(await reader.closed).toBeInstanceOf(Unsupported);
	// Mixed demand there is just the payload.
	const meta = hops.subscribe(true);
	const payload = hops.subscribe(false);
	await hops.asked(false);
	writeGroup(hops.track, 0n, [3]).finish();
	expect((await payload.recvGroup())?.sequence).toBe(0n);
	expect((await meta.recvGroup())?.meta).toBe(true);
	meta.close();
	payload.close();
	await hops.close();
});

test("moq-transport refuses metadata-only demand", async () => {
	const pair = createMockTransportPair(Ietf.ALPN.DRAFT_18);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcast = origin.route(path).broadcast;
	broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const remote = wireOf(client).consume(path);
	const reader = remote.track("video").subscribe({ meta: true });
	expect(await reader.closed).toBeInstanceOf(Unsupported);
	remote.close();
	broadcast.close();
	client.abort();
	server.abort();
});

test("an update clearing Meta at a payload position re-serves that group from its frame", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = origin.route(path).broadcast;
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	writeGroup(track, 0n, [3, 4, 5]).finish();

	const client = await Stream.open(pair.client, { version });
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	incoming.releaseLock();
	if (accepted.done) throw new Error("publisher never accepted the subscribe stream");
	const server = new Stream({ readable: accepted.value.readable, writable: accepted.value.writable, version });
	const msg = new Subscribe({ id: 0n, broadcast: path, track: "video", priority: 0, maxDelay: 30_000, meta: true });
	const serving = publisher.runSubscribe(msg, server);
	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	// Each Group Stream as the subscriber reads it: its header, and its frames' sizes.
	const served = async () => {
		const next = await opened.read();
		if (next.done) throw new Error("no group stream");
		const reader = new Reader(next.value, undefined, version);
		await reader.u53();
		const header = await GroupMessage.decode(reader, version);
		const group = new GroupProducer(header.sequence);
		hooks.startGroup(group, header.frameStart);
		if (header.meta) hooks.metaGroup(group);
		await readFrames(reader, group, { scale: 1000, lengthFirst: true, meta: header.meta });
		group.finish();
		const consumer = group.consume();
		const sizes: number[] = [];
		for (let frame = await consumer.nextFrame(); frame; frame = await consumer.nextFrame()) sizes.push(frame.size);
		return { sequence: header.sequence, frameStart: header.frameStart, meta: header.meta, sizes };
	};

	expect(await served()).toEqual({ sequence: 0n, frameStart: 0, meta: true, sizes: [3, 4, 5] });
	// The subscriber holds frame 0's payload already, from elsewhere: it asks from frame 1.
	await new SubscribeUpdate({
		priority: 0,
		maxDelay: 30_000,
		ranges: [],
		update: 1n,
		meta: false,
		payload: { group: 0n, frame: 1 },
	}).encode(client.writer, version);
	expect(await served()).toEqual({ sequence: 0n, frameStart: 1, meta: false, sizes: [4, 5] });

	client.close();
	broadcast.close();
	await serving;
	pair.client.close();
	pair.server.close();
});
