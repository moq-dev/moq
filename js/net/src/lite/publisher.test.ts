import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { Producer as GroupProducer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { randomOrigin } from "./origin.ts";
import { sendOrder } from "./priority.ts";
import { Publisher } from "./publisher.ts";
import { decodeSubscribeResponse, Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { ALPN_05, ALPN_06_WIP, Version } from "./version.ts";

// Delivers `sequences` in the given order, finishes the track, and returns the
// SUBSCRIBE_END boundary the publisher put on the wire.
async function subscribeEnd(sequences: number[]): Promise<number> {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 });
	void publisher.runSubscribe(msg, server);

	// Finish the track only once it's being served, so the publisher observes a live
	// track ending rather than resolving a subscribe against an already-closed one.
	for (const sequence of sequences) {
		const group = new GroupProducer(sequence);
		group.writeString("hello");
		group.close();
		track.writeGroup(group);

		// Let the publisher drain this group before the next, so arrival order is the
		// order given rather than whatever the cache hands over in one batch. A group that
		// arrives late opens no stream at all, so there is nothing firmer to wait on.
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	track.close();

	try {
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_05);
			if ("end" in resp) return resp.end.group;
		}
	} finally {
		publisher.close();
		client.close();
	}
}

// Wait for the publisher to start serving the next group.
//
// The peer end of the stream arrives while the publisher is still opening it, so that alone
// says nothing about the ranking. Its first byte does: the header is written only after the
// group has joined the subscription's ranking.
async function servingNextGroup(opened: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>) {
	const next = await opened.read();
	if (next.done) throw new Error("publisher never opened the group stream");

	const reader = next.value.getReader();
	if ((await reader.read()).done) throw new Error("publisher never wrote the group header");
	reader.releaseLock();
}

// Serves `sequences` under the given subscription, optionally raising its priority via
// SUBSCRIBE_UPDATE after the first group, and returns the send order of each group stream in
// the order the publisher opened them.
//
// Every group is left open, so all of their streams are in flight and ranked against each
// other. A group that finished would leave the ranking, which is the point of it.
async function groupSendOrders(options: { priority: number; sequences: number[]; update?: number; ordered?: boolean }) {
	const { priority, sequences, update, ordered } = options;
	const groups = sequences.map((sequence) => new GroupProducer(sequence));
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority, ordered });
	void publisher.runSubscribe(msg, server);

	// The peer end of each group stream, which arrives as the publisher opens it.
	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	try {
		for (const [index, group] of groups.entries()) {
			if (index === 1 && update !== undefined) {
				await new SubscribeUpdate({ priority: update, ordered }).encode(client.writer, Version.DRAFT_05);
				// Wait for the publisher to apply it, rather than assuming it beat the next group.
				while (track.subscription.peek()?.priority !== update) await track.subscription.changed();
			}

			group.writeString("hello");
			track.writeGroup(group);

			// One stream per group, in the order given: wait for this one before queuing the next.
			await servingNextGroup(opened);
		}

		return pair.server.sendStreams.uni.map((stream) => {
			if (stream.sendOrder === undefined) throw new Error("group stream opened without a send order");
			return stream.sendOrder;
		});
	} finally {
		opened.releaseLock();
		for (const group of groups) group.close();
		publisher.close();
		client.close();
	}
}

// Without a send order every group stream ranks the same, so the transport round-robins them
// and a stalled low-priority track steals bandwidth from the one the subscriber asked for.
// Live playback wants the newest group, so it is the one at position 0.
test("lite draft-05: group streams are ranked newest-first", async () => {
	const orders = await groupSendOrders({ priority: 7, sequences: [0, 1, 2] });
	expect(orders).toEqual([
		sendOrder({ priority: 7, position: 2 }),
		sendOrder({ priority: 7, position: 1 }),
		sendOrder({ priority: 7, position: 0 }),
	]);

	expect(orders[2]).toBeGreaterThan(orders[1]);
});

// An ordered subscriber is playing through in sequence, so the oldest group in flight is the
// one it needs next.
test("lite draft-05: an ordered subscription is ranked oldest-first", async () => {
	const orders = await groupSendOrders({ priority: 7, sequences: [0, 1, 2], ordered: true });
	expect(orders).toEqual([
		sendOrder({ priority: 7, position: 0 }),
		sendOrder({ priority: 7, position: 1 }),
		sendOrder({ priority: 7, position: 2 }),
	]);

	expect(orders[2]).toBeLessThan(orders[1]);
});

// Two tracks the subscriber values equally each get their next group out, whatever their group
// numbering: ranking by sequence would let the one with larger numbers starve the other.
test("lite draft-05: equal priorities tie regardless of group numbering", async () => {
	const [fresh, ongoing] = await Promise.all([
		groupSendOrders({ priority: 7, sequences: [0, 1] }),
		groupSendOrders({ priority: 7, sequences: [900_000, 900_001] }),
	]);

	expect(fresh).toEqual(ongoing);
});

// SUBSCRIBE_UPDATE re-ranks the subscription, so every group it is serving picks up the new
// priority, including one opened after the update landed.
test("lite draft-05: a subscribe update re-ranks the whole subscription", async () => {
	const orders = await groupSendOrders({ priority: 1, sequences: [0, 1], update: 9 });
	expect(orders).toEqual([sendOrder({ priority: 9, position: 1 }), sendOrder({ priority: 9, position: 0 })]);
});

// A group can outlive the priority it opened with, so an update has to reach the stream that
// is already on the wire. Otherwise (say) an active-speaker change waits for the next group.
test("lite draft-05: a subscribe update re-ranks a group already on the wire", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 1 });
	void publisher.runSubscribe(msg, server);

	// Leave the group open, so its stream is still being served when the update lands.
	const group = new GroupProducer(4);
	group.writeString("hello");
	track.writeGroup(group);

	const opened = pair.client.incomingUnidirectionalStreams.getReader();
	await servingNextGroup(opened);
	opened.releaseLock();

	// The only group in flight, so it is the one to send next.
	const stream = pair.server.sendStreams.uni[0];
	expect(stream.sendOrder).toBe(sendOrder({ priority: 1 }));

	try {
		await new SubscribeUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		// The publisher re-ranks from that same signal, so wait on the send order itself rather
		// than on the dispatch order between its subscriber and this one.
		for (let i = 0; i < 200 && stream.sendOrder === sendOrder({ priority: 1 }); i++) {
			await new Promise((resolve) => setTimeout(resolve, 5));
		}

		expect(stream.sendOrder).toBe(sendOrder({ priority: 9 }));
	} finally {
		group.close();
		publisher.close();
		client.close();
	}
});

// Opening a stream can block on transport capacity, and a subscription listener only sees
// later changes, so an update landing in that window would otherwise be lost until the next one.
test("lite draft-05: a subscribe update during the stream open still ranks the group", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	// Hold the group's stream open call until the test releases it.
	let release: () => void = () => {};
	const opening = new Promise<void>((resolve) => {
		release = resolve;
	});
	const open = pair.server.createUnidirectionalStream.bind(pair.server);
	pair.server.createUnidirectionalStream = async (options) => {
		await opening;
		return open(options);
	};

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 1 });
	void publisher.runSubscribe(msg, server);

	const group = new GroupProducer(4);
	group.writeString("hello");
	track.writeGroup(group);

	try {
		// The publisher is now parked inside the open, having already read priority 1.
		await new SubscribeUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		release();
		const opened = pair.client.incomingUnidirectionalStreams.getReader();
		if ((await opened.read()).done) throw new Error("publisher never opened the group stream");
		opened.releaseLock();

		for (let i = 0; i < 200 && pair.server.sendStreams.uni[0]?.sendOrder !== sendOrder({ priority: 9 }); i++) {
			await new Promise((resolve) => setTimeout(resolve, 5));
		}

		expect(pair.server.sendStreams.uni[0]?.sendOrder).toBe(sendOrder({ priority: 9 }));
	} finally {
		group.close();
		publisher.close();
		client.close();
	}
});

// A stalled track piles up open groups, and the signals leak guard throws at 100 subscribers
// on one signal, so the subscription is ranked by a single listener rather than one per group.
test("lite draft-05: many concurrent groups share one subscription listener", async () => {
	const count = 120;
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 1 });
	void publisher.runSubscribe(msg, server);

	// Leave every group open, so all of their streams are in flight at once.
	const groups = Array.from({ length: count }, (_, sequence) => new GroupProducer(sequence));
	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	try {
		for (const group of groups) {
			group.writeString("hello");
			track.writeGroup(group);
			await servingNextGroup(opened);
		}
		opened.releaseLock();

		expect(pair.server.sendStreams.uni.length).toBe(count);

		await new SubscribeUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		// Newest-first, so the last group opened is at position 0 and the first is at the back.
		const expected = groups.map((group) => sendOrder({ priority: 9, position: count - 1 - group.sequence }));
		for (let i = 0; i < 200; i++) {
			if (pair.server.sendStreams.uni.every((stream, at) => stream.sendOrder === expected[at])) break;
			await new Promise((resolve) => setTimeout(resolve, 5));
		}

		expect(pair.server.sendStreams.uni.map((stream) => stream.sendOrder)).toEqual(expected);
	} finally {
		for (const group of groups) group.close();
		publisher.close();
		client.close();
	}
});

// A send order only schedules the local end, so the subscriber's FETCH stream ranks its own
// request; without this the response competes with the group streams at the default order.
test("lite draft-05: the fetch response ranks the publisher's own writes", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const group = new GroupProducer(7);
	group.writeString("hello");
	group.close();
	track.writeGroup(group);

	const client = await Stream.open(pair.client);

	// Accept by hand rather than via Stream.accept, so the test keeps the writable the
	// publisher ranks (a real WebTransportSendStream takes the same assignment).
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	incoming.releaseLock();
	if (accepted.done) throw new Error("publisher never saw the fetch stream");
	const server = new Stream(accepted.value);

	const msg = new Fetch({ broadcast: Path.from("test"), track: "video", priority: 3, group: 7 });
	try {
		await publisher.runFetch(msg, server);

		expect((accepted.value.writable as { sendOrder?: number }).sendOrder).toBe(sendOrder({ priority: 3 }));
	} finally {
		publisher.close();
		client.close();
	}
});

// A Rust subscriber feeds this value straight into `track::Producer::finish_at`, which is
// exclusive, so an inclusive bound here silently truncates the final group across languages.
test("lite draft-05: subscribe end is the exclusive boundary", async () => {
	expect(await subscribeEnd([0, 1, 2])).toBe(3);
});

// recvGroup is arrival-ordered, so the boundary has to clear the max sequence delivered,
// not the last one seen. Otherwise the boundary lands on a group already on the wire.
test("lite draft-05: subscribe end clears the max sequence when groups arrive out of order", async () => {
	expect(await subscribeEnd([0, 2, 1])).toBe(3);
});

// 0 is the only encoding for "no groups at all"; an inclusive bound cannot express it
// without colliding with a track whose sole group was sequence 0.
test("lite draft-05: subscribe end is 0 when no groups were produced", async () => {
	expect(await subscribeEnd([])).toBe(0);
});

/** One group stream the publisher put on the wire. */
type Served = { sequence: number; frameStart: number; payloads: string[] };

/**
 * Serves `groups` (frame payloads per group, keyed by sequence) under `bounds`, and
 * reports every group stream that reached the wire plus the resolved
 * SUBSCRIBE_START / SUBSCRIBE_END range.
 */
async function serve(
	groups: Record<number, string[]>,
	bounds: { startGroup?: number; startFrame?: number; endGroup?: number; endFrame?: number },
): Promise<{ start?: number; end?: number; served: Served[] }> {
	const pair = createMockTransportPair(ALPN_06_WIP);
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: bounds.startGroup,
		startFrame: bounds.startFrame ?? 0,
		endGroup: bounds.endGroup,
		endFrame: bounds.endFrame,
	});
	void publisher.runSubscribe(msg, server);

	for (const [sequence, frames] of Object.entries(groups)) {
		const group = new GroupProducer(Number(sequence));
		for (const frame of frames) group.writeString(frame);
		group.close();
		track.writeGroup(group);

		// Let the publisher drain each group before the next, so the streams arrive in
		// the order written rather than whatever the cache hands over in one batch.
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	track.close();

	const reader = pair.client.incomingUnidirectionalStreams.getReader();
	try {
		const served: Served[] = [];
		for (;;) {
			// The publisher may simply have nothing more to send, so race the read
			// against an idle timeout. Clear the timer either way, and cancel rather
			// than release below: releasing with a read still pending would throw.
			let timer: ReturnType<typeof setTimeout> | undefined;
			const idle = new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), 50);
			});
			const next = await Promise.race([reader.read(), idle]);
			clearTimeout(timer);
			if (!next || next.done) break;

			const stream = new Reader(next.value);
			await stream.u53(); // stream type
			const header = await GroupMessage.decode(stream, Version.DRAFT_06);

			const payloads: string[] = [];
			while (!(await stream.done())) {
				// Each frame is a zigzag timestamp delta, then a length-prefixed payload.
				await stream.u62();
				payloads.push(new TextDecoder().decode(await stream.read(await stream.u53())));
			}
			served.push({ sequence: header.sequence, frameStart: header.frameStart, payloads });
		}

		let start: number | undefined;
		let end: number | undefined;
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_06);
			if ("start" in resp) start = resp.start.group;
			if ("end" in resp) {
				end = resp.end.group;
				break;
			}
		}
		return { start, end, served };
	} finally {
		// Settles the pending read as well as dropping the stream.
		await reader.cancel();
		broadcast.close();
		client.close();
	}
}

// The response has no per-frame index, so the receiver numbers what it gets from the
// GROUP header. Ignoring the requested start would relabel frame 0 as frame N.
test("lite draft-06: a subscription starting mid-group skips the head", async () => {
	const { served } = await serve({ 0: ["a", "b", "c", "d"] }, { startGroup: 0, startFrame: 2 });
	expect(served).toEqual([{ sequence: 0, frameStart: 2, payloads: ["c", "d"] }]);
});

// The end bound is inclusive.
test("lite draft-06: a subscription capped mid-group stops at the end frame", async () => {
	const { served } = await serve(
		{ 0: ["a", "b", "c", "d"] },
		{ startGroup: 0, startFrame: 1, endGroup: 0, endFrame: 2 },
	);
	expect(served).toEqual([{ sequence: 0, frameStart: 1, payloads: ["b", "c"] }]);
});

// The default is the whole group, byte-identical to a draft with no such field.
test("lite draft-06: an unbounded subscription serves the whole group", async () => {
	const { served } = await serve({ 0: ["a", "b"] }, {});
	expect(served).toEqual([{ sequence: 0, frameStart: 0, payloads: ["a", "b"] }]);
});

// The frame bounds qualify their own group and nothing else. Without a span like this,
// an implementation that caps every group passes just as well.
test("lite draft-06: frame bounds apply only to the groups they name", async () => {
	const { served } = await serve(
		{ 0: ["a0", "a1", "a2"], 1: ["b0", "b1", "b2"], 2: ["c0", "c1", "c2"] },
		{ startGroup: 0, startFrame: 2, endGroup: 2, endFrame: 0 },
	);
	expect(served).toEqual([
		{ sequence: 0, frameStart: 2, payloads: ["a2"] },
		// Between the bounds: served whole, from frame 0, with no cap.
		{ sequence: 1, frameStart: 0, payloads: ["b0", "b1", "b2"] },
		{ sequence: 2, frameStart: 0, payloads: ["c0"] },
	]);
});

// A group below the requested start was never asked for. Serving it also lets
// SUBSCRIBE_START name a group below the start, which the draft forbids.
test("lite draft-06: groups below the start group are not served", async () => {
	const { start, served } = await serve({ 0: ["x"], 1: ["y"], 2: ["z"] }, { startGroup: 1 });
	expect(served.map((s) => s.sequence)).toEqual([1, 2]);
	expect(start).toBe(1);
});

// The end group is inclusive; anything past it is outside the subscription.
test("lite draft-06: groups past the end group are not served", async () => {
	const { served } = await serve({ 0: ["w"], 1: ["x"], 2: ["y"], 3: ["z"] }, { startGroup: 1, endGroup: 2 });
	expect(served.map((s) => s.sequence)).toEqual([1, 2]);
});

// SUBSCRIBE_END names the track's exclusive final boundary, not the capped delivered
// range: a Rust peer feeds it into finish_at, so a truncated value would silently drop
// the held-back groups across languages.
test("lite draft-06: a capped subscription still reports the track's final boundary", async () => {
	const { end } = await serve({ 0: ["w"], 1: ["x"], 2: ["y"], 3: ["z"] }, { startGroup: 1, endGroup: 2 });
	expect(end).toBe(4);
});

// Group streams open with waitUntilAvailable, so a browser at its concurrent stream cap
// parks the open until the peer frees a slot. That can outlast the subscription, and the
// queued group holds its frames the whole time.
// Serves one finished group to a subscriber over a transport that has no stream slot free,
// so the group sits queued inside the open until `freeSlot` is called. `outcome` settles
// with what became of the group once the slot frees, so no test has to guess at a delay.
async function saturatedGroup() {
	const pair = createMockTransportPair(ALPN_05);

	let freeSlot!: () => void;
	const slot = new Promise<void>((resolve) => {
		freeSlot = resolve;
	});

	let opening!: () => void;
	const opened = new Promise<void>((resolve) => {
		opening = resolve;
	});

	let reset!: () => void;
	let wrote!: () => void;
	const outcome = Promise.race([
		new Promise<"reset">((resolve) => {
			reset = () => resolve("reset");
		}),
		new Promise<"sent">((resolve) => {
			wrote = () => resolve("sent");
		}),
	]);

	const groupStream = new WritableStream<Uint8Array>({ write: () => wrote(), abort: () => reset() });

	// Stand in for a transport at its stream cap, which parks the open the way a browser does
	// rather than rejecting it, whatever we asked for.
	const requested: unknown[] = [];
	pair.server.createUnidirectionalStream = async (options?: unknown) => {
		requested.push(options);
		opening();
		await slot;
		return groupStream;
	};

	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 });
	void publisher.runSubscribe(msg, server);

	// A finished group still has frames to send, so its own close must not drop it.
	const group = new GroupProducer(0);
	group.writeString("hello");
	group.close();
	track.writeGroup(group);

	// The group is queued inside the open from here on.
	await opened;

	return {
		client,
		track,
		freeSlot,
		outcome,
		requested,
		close: () => {
			publisher.close();
			broadcast.close();
		},
	};
}

// Group streams are the one path that must not queue behind the peer's stream limit: the
// transport serves queued opens oldest-first, which is backwards for live media, and an
// open already handed to it can't be taken back.
test("lite draft-05: group streams do not ask the transport to wait for a slot", async () => {
	const { requested, freeSlot, close } = await saturatedGroup();

	expect(requested).toEqual([{ sendOrder: expect.any(Number), waitUntilAvailable: false }]);

	freeSlot();
	close();
});

test("lite draft-05: a group waiting for a stream slot is dropped when the subscriber leaves", async () => {
	const { client, track, freeSlot, outcome, close } = await saturatedGroup();

	client.close();

	// Seeing the close is what cancels the queued open, so wait for the publisher to drop
	// the subscription rather than racing it against the slot below.
	while (track.subscription.peek() !== undefined) await track.subscription.changed();

	freeSlot();
	expect(await outcome).toBe("reset");

	close();
});

// The publisher FINs the subscribe stream itself once a track ends, which must not be
// mistaken for the subscriber leaving: SUBSCRIBE_END counts those queued groups as
// delivered, so dropping them here would strand the tail of every finite track.
test("lite draft-05: a group waiting for a stream slot survives the track finishing", async () => {
	const { client, track, freeSlot, outcome, close } = await saturatedGroup();

	track.close();

	// Read to the FIN the publisher sends after SUBSCRIBE_END. That FIN is the moment a
	// cancel keyed on our own close would fire, so the slot must not free up before it.
	for (;;) {
		const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_05);
		if ("end" in resp) break;
	}
	await client.reader.closed;

	freeSlot();
	expect(await outcome).toBe("sent");

	close();
});
