// Group order end to end: what a subscriber asks for reaches the publisher, and the publisher's
// group streams carry send orders that put the requested group first.
//
// The mock transport delivers streams in creation order and ignores send order, so the send
// order each stream was assigned is the observable. Every group is left open, so all of their
// streams stay in flight and ranked against each other.

import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { accept, connect, type Established } from "./connection/index.ts";
import type { Producer as GroupProducer } from "./group.ts";
import { FetchHeader } from "./ietf/fetch.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport, textFrame } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { sendOrder } from "./priority.ts";
import type { SendStream } from "./stream.ts";
import { Timescale } from "./time.ts";
import type { Order, Producer as TrackProducer, Subscriber as TrackSubscriber } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

// A budget covering every group these tests write, so expiry never decides what is sent.
const MAX_DELAY = Milli(30_000);

// The send orders a subscription at priority 0 gives its streams, in the order they opened,
// when stream `i` carries group `i` of `count` all still in flight.
function expected(order: Order, count: number): number[] {
	return Array.from({ length: count }, (_, sequence) =>
		sendOrder({ priority: 0, position: order === "ascending" ? sequence : count - 1 - sequence }),
	);
}

// The positions in `sendStreams.uni` of each transport's fetch streams.
const fetchStreams = new WeakMap<MockTransport, Set<number>>();

// Note which uni streams open with a fetch header. A draft-18 subscriber that joins after
// Largest fetches the group's head on one, ranked with that group, beside its group streams.
function noteFetchStreams(transport: MockTransport): void {
	const fetches = new Set<number>();
	fetchStreams.set(transport, fetches);
	const create = transport.createUnidirectionalStream.bind(transport);
	transport.createUnidirectionalStream = async (options) => {
		const stream = await create(options);
		// Kept by position, since a wrapper such as holdFins replaces the logged stream.
		const index = transport.sendStreams.uni.indexOf(stream as SendStream);
		const getWriter = stream.getWriter.bind(stream);
		stream.getWriter = () => {
			const writer = getWriter();
			const write = writer.write.bind(writer);
			let first = true;
			writer.write = (chunk) => {
				if (first && chunk?.[0] === FetchHeader.type) fetches.add(index);
				first = false;
				return write(chunk);
			};
			return writer;
		};
		return stream;
	};
}

// The send orders of a transport's group streams. Protocol streams, such as the setup stream,
// open without one, and fetch streams are left out.
function sendOrders(transport: MockTransport): number[] {
	const fetches = fetchStreams.get(transport);
	return transport.sendStreams.uni.flatMap((stream, index) =>
		stream.sendOrder === undefined || fetches?.has(index) ? [] : [stream.sendOrder],
	);
}

// A group with one frame, left open.
function writeGroup(track: TrackProducer): GroupProducer {
	const group = track.appendGroup();
	group.writeFrame(textFrame("x"));
	return group;
}

// Receiving a group means its stream header was written, which the publisher does only after
// ranking the stream.
async function receive(subscriber: TrackSubscriber, count: number): Promise<number[]> {
	const sequences: number[] = [];
	for (let i = 0; i < count; i++) {
		const group = await subscriber.recvGroup();
		if (!group) throw new Error("track ended early");
		sequences.push(group.sequence);
	}
	return sequences;
}

// Resolve once the producer's aggregate subscription satisfies `predicate`.
async function aggregate(track: TrackProducer, predicate: (order: Order | undefined) => boolean): Promise<void> {
	while (!predicate(track.subscription.peek()?.order)) await track.subscription.changed();
}

interface Session {
	client: Established;
	server: Established;
	transport: MockTransport;
}

async function session(protocol: string, origin: OriginProducer): Promise<Session> {
	const pair = createMockTransportPair(protocol);
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	noteFetchStreams(pair.server);
	return { client, server, transport: pair.server };
}

// A backlog already cached when the subscription arrives. The publisher's own reader hands a
// buffered backlog out lowest first, so stream `i` carries group `i`. With `update`, the
// subscriber switches to that order once every group is in flight.
async function backlog(protocol: string, order: Order, update?: Order) {
	const origin = new OriginProducer();
	const { client, server, transport } = await session(protocol, origin);

	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const groups = [0, 1, 2].map(() => writeGroup(video));

	const remote = wireOf(client).consume(Path.from("test"));
	const subscriber = remote.track("video").subscribe({
		maxDelay: MAX_DELAY,
		order,
		groups: { start: { included: 0 } },
	});
	await receive(subscriber, groups.length);

	if (update !== undefined) {
		subscriber.update({ maxDelay: MAX_DELAY, order: update, groups: { start: { included: 0 } } });
		await aggregate(video, (upstream) => upstream === update);
	}

	const result = { upstream: video.subscription.peek()?.order, orders: sendOrders(transport) };

	for (const group of groups) group.finish();
	subscriber.close();
	remote.close();
	broadcast.close();
	client.abort();
	server.abort();
	return result;
}

test.each(["ascending", "descending"] as const)("lite-07 serves a backlog %s", async (order) => {
	const { upstream, orders } = await backlog(Lite.ALPN_07_WIP, order);
	expect(upstream).toBe(order);
	expect(orders).toEqual(expected(order, 3));
});

// SUBSCRIBE_UPDATE re-ranks the groups already on the wire, not only the ones opened after.
test("lite-07 re-ranks a backlog in flight when the order changes", async () => {
	const { orders } = await backlog(Lite.ALPN_07_WIP, "descending", "ascending");
	expect(orders).toEqual(expected("ascending", 3));
});

// Lite-06 has no field for it, so the publisher never hears the request and serves newest first.
test("lite-06 serves an ascending subscription newest first", async () => {
	const { upstream, orders } = await backlog(Lite.ALPN_06, "ascending");
	expect(upstream).toBe("descending");
	expect(orders).toEqual(expected("descending", 3));
});

// moq-transport joins at the live edge, so the groups arrive live, one at a time.
test.each(["ascending", "descending"] as const)("ietf serves GROUP_ORDER %s", async (order) => {
	const origin = new OriginProducer();
	const { client, server, transport } = await session(Ietf.ALPN.DRAFT_18, origin);

	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = wireOf(client).consume(Path.from("test"));
	const subscriber = remote.track("video").subscribe({ maxDelay: MAX_DELAY, order });
	await aggregate(video, (upstream) => upstream === order);

	const groups: GroupProducer[] = [];
	for (let i = 0; i < 3; i++) {
		groups.push(writeGroup(video));
		await receive(subscriber, 1);
	}
	expect(sendOrders(transport)).toEqual(expected(order, 3));

	for (const group of groups) group.finish();
	subscriber.close();
	remote.close();
	broadcast.close();
	client.abort();
	server.abort();
});

// Holds the FIN of every group stream `transport` opens from here on, while their data still
// flows, until `release`. `closing` resolves once a stream has asked to close.
function holdFins(transport: MockTransport): { closing: Promise<void>; release: () => void } {
	let release!: () => void;
	const released = new Promise<void>((resolve) => {
		release = resolve;
	});
	let closed!: () => void;
	const closing = new Promise<void>((resolve) => {
		closed = resolve;
	});

	const create = transport.createUnidirectionalStream.bind(transport);
	transport.createUnidirectionalStream = async (options) => {
		const inner = await create(options);
		const index = transport.sendStreams.uni.indexOf(inner as SendStream);
		const writer = inner.getWriter();
		const outer: SendStream = new WritableStream<Uint8Array>({
			write: (chunk) => writer.write(chunk),
			close: async () => {
				// A join's fetch stream closes as usual; only group streams are held.
				if (fetchStreams.get(transport)?.has(index)) return writer.close();
				closed();
				await released;
				await writer.close();
			},
			abort: (reason) => writer.abort(reason),
		});
		outer.sendOrder = options?.sendOrder;
		// Record the stream the publisher re-ranks.
		const log = transport.sendStreams.uni;
		log[index] = outer;
		return outer;
	};
	return { closing, release };
}

// A finished group whose FIN is still queued keeps its rank, rather than being frozen while
// the groups behind it are promoted to tie with it.
test("ietf keeps a closing group stream ranked until it settles", async () => {
	const origin = new OriginProducer();
	const { client, server, transport } = await session(Ietf.ALPN.DRAFT_18, origin);
	const fins = holdFins(transport);

	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const remote = wireOf(client).consume(Path.from("test"));
	const subscriber = remote.track("video").subscribe({ maxDelay: MAX_DELAY, order: "descending" });
	await aggregate(video, (upstream) => upstream === "descending");

	const groups: GroupProducer[] = [];
	for (let i = 0; i < 3; i++) {
		groups.push(writeGroup(video));
		await receive(subscriber, 1);
	}

	// The newest group, ranked first, finishes while its FIN is held.
	groups[2]?.finish();
	await fins.closing;
	expect(sendOrders(transport)).toEqual(expected("descending", 3));

	fins.release();
	for (const group of groups) group.finish();
	subscriber.close();
	remote.close();
	broadcast.close();
	client.abort();
	server.abort();
});

// One track fanned out to an ascending and a descending subscriber, the shape a relay serves
// from one upstream request (a JS app does not proxy, so the track is local here). The
// aggregate a wire would request upstream turns descending once the descending subscriber
// joins, so it is never held behind the other's backlog, and each session still serves its
// own subscriber in its own order.
test("a shared track aggregates descending and serves each subscriber in its own order", async () => {
	const origin = new OriginProducer();
	const [ascending, descending] = await Promise.all([
		session(Lite.ALPN_07_WIP, origin),
		session(Lite.ALPN_07_WIP, origin),
	]);

	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const ascendingRemote = wireOf(ascending.client).consume(Path.from("test"));
	const ascendingTrack = ascendingRemote.track("video").subscribe({ maxDelay: MAX_DELAY, order: "ascending" });
	await aggregate(video, (order) => order === "ascending");

	const descendingRemote = wireOf(descending.client).consume(Path.from("test"));
	const descendingTrack = descendingRemote.track("video").subscribe({ maxDelay: MAX_DELAY, order: "descending" });
	await aggregate(video, (order) => order === "descending");

	// One at a time, so each session opens its streams in sequence order.
	const groups: GroupProducer[] = [];
	for (let i = 0; i < 3; i++) {
		groups.push(writeGroup(video));
		await Promise.all([receive(ascendingTrack, 1), receive(descendingTrack, 1)]);
	}

	expect(sendOrders(ascending.transport)).toEqual(expected("ascending", 3));
	expect(sendOrders(descending.transport)).toEqual(expected("descending", 3));

	for (const group of groups) group.finish();
	ascendingTrack.close();
	descendingTrack.close();
	ascendingRemote.close();
	descendingRemote.close();
	broadcast.close();
	for (const { client, server } of [ascending, descending]) {
		client.abort();
		server.abort();
	}
});
