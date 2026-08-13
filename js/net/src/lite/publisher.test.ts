import { expect, test } from "bun:test";
import { Producer as GroupProducer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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

// How long a served-subscription test waits for the next group stream before calling the
// publisher idle. Nothing waits this out: it only bounds the read when a group never comes.
const IDLE_MS = 500;

// One macrotask turn, which drains every microtask queued behind it. Signal notifications
// are coalesced per microtask, so this is what lets a just-written group reach the serving
// loop's armed `recvGroup` without a test guessing at a delay.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

// Wraps a writable so its writes park until `release()`, giving a test a window inside
// whatever the publisher is writing while its other loops keep running. `parked` settles on
// the first write attempt, held or not.
function gateWrites(target: WritableStream<Uint8Array>, hold: boolean) {
	const writer = target.getWriter();

	let release!: () => void;
	const released = new Promise<void>((resolve) => {
		release = resolve;
	});
	if (!hold) release();

	let parking!: () => void;
	const parked = new Promise<void>((resolve) => {
		parking = resolve;
	});

	const writable = new WritableStream<Uint8Array>({
		async write(chunk) {
			parking();
			await released;
			await writer.write(chunk);
		},
		close: () => writer.close(),
		abort: (err) => writer.abort(err),
	});

	return { writable, parked, release };
}

// Opens a served subscription and returns the machinery to write groups and observe
// which of them the publisher put on the wire (each group gets its own uni stream).
//
// `gated` holds the publisher's subscribe-stream writes until `release()`, parking the
// serving loop mid-iteration so a test can drive the concurrent SUBSCRIBE_UPDATE loop
// against a group the loop is already holding.
async function servedSubscription(
	options: {
		startGroup?: number;
		endGroup?: number;
		endFrame?: number;
		gated?: boolean;
		// Frame payloads written into every served group. Frame bounds need draft-06.
		frames?: string[];
		version?: Version;
	} = {},
) {
	const version = options.version ?? Version.DRAFT_05;
	const frames = options.frames ?? ["hello"];
	const pair = createMockTransportPair(version === Version.DRAFT_06 ? ALPN_06_WIP : ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

	const client = await Stream.open(pair.client);

	// Accept by hand rather than via Stream.accept, so the gate sits between the publisher
	// and the wire.
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	incoming.releaseLock();
	if (accepted.done) throw new Error("publisher never accepted the subscribe stream");

	const gate = gateWrites(accepted.value.writable, options.gated ?? false);
	const server = new Stream({ readable: accepted.value.readable, writable: gate.writable });

	const msg = new Subscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: options.startGroup,
		endGroup: options.endGroup,
		endFrame: options.endFrame,
	});
	void publisher.runSubscribe(msg, server);

	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	return {
		client,
		track,
		parked: gate.parked,
		release: gate.release,
		serve(sequence: number) {
			const group = new GroupProducer(sequence);
			for (const frame of frames) group.writeString(frame);
			group.close();
			track.writeGroup(group);
		},
		// The next group stream the publisher opened, drained, or undefined once it has gone
		// idle. A group the publisher dropped then fails an assertion instead of hanging the
		// test on a stream that will never arrive.
		async servedGroup(): Promise<Served | undefined> {
			let timer: ReturnType<typeof setTimeout> | undefined;
			const idle = new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), IDLE_MS);
			});
			const next = await Promise.race([opened.read(), idle]);
			clearTimeout(timer);
			if (!next || next.done) return undefined;

			const reader = new Reader(next.value);
			await reader.u53(); // stream type
			const header = await GroupMessage.decode(reader, version);

			const payloads: string[] = [];
			while (!(await reader.done())) {
				// Each frame is a zigzag timestamp delta, then a length-prefixed payload.
				await reader.u62();
				payloads.push(new TextDecoder().decode(await reader.read(await reader.u53())));
			}
			return { sequence: header.sequence, frameStart: header.frameStart, payloads };
		},
		// The sequence alone, for tests that only care which groups reached the wire.
		async servedSequence(): Promise<number | undefined> {
			return (await this.servedGroup())?.sequence;
		},
		async close() {
			// Settles any pending read as well as dropping the stream.
			await opened.cancel();
			publisher.close();
			client.close();
		},
	};
}

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("lite draft-05: a late-arriving older group is still served", async () => {
	const sub = await servedSubscription({ startGroup: 1 });
	try {
		// Serve one at a time so arrival order at the publisher is the order given.
		sub.serve(1);
		expect(await sub.servedSequence()).toBe(1);
		sub.serve(3);
		expect(await sub.servedSequence()).toBe(3);

		// Group 2 lands after group 3 was already served.
		sub.serve(2);
		expect(await sub.servedSequence()).toBe(2);
	} finally {
		await sub.close();
	}
});

// SUBSCRIBE_START promises nothing below the announced sequence will be delivered, so
// the floor is pinned there: a straggler below the first served group is dropped even
// though arrival-order serving would otherwise surface it.
test("lite draft-05: a straggler below the announced start group is not served", async () => {
	const sub = await servedSubscription();
	try {
		sub.serve(2);
		expect(await sub.servedSequence()).toBe(2);

		// The publisher announced group 2 as the resolved start.
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_05);
		if (!("start" in resp)) throw new Error("expected SUBSCRIBE_START");
		expect(resp.start.group).toBe(2);

		// A straggler below the announced start never reaches the wire: the next
		// stream the publisher opens is group 3's.
		sub.serve(1);
		sub.serve(3);
		expect(await sub.servedSequence()).toBe(3);
	} finally {
		await sub.close();
	}
});

// The serving loop prefetches the next group the moment it takes one, so a SUBSCRIBE_UPDATE
// can lower the cap while a group is already in hand. Dropping it there would be permanent:
// the group has left the buffer, so raising the cap again could never bring it back. The cap
// gates what the read cursor hands out, and a group already handed out stays served.
test("lite draft-05: a group taken before the cap dropped is still served", async () => {
	const sub = await servedSubscription({ startGroup: 0, gated: true });
	try {
		// Group 0 parks the loop inside its SUBSCRIBE_START write, prefetch already armed.
		sub.serve(0);
		await sub.parked;

		// So group 1 leaves the buffer here; only the parked loop still holds it.
		sub.serve(1);
		await flush();

		// Cap the subscription at group 0, behind the loop's back.
		await new SubscribeUpdate({ priority: 0, endGroup: 0 }).encode(sub.client.writer, Version.DRAFT_05);
		while (sub.track.subscription.peek()?.endGroup !== 0) await sub.track.subscription.changed();

		sub.release();

		// Both reach the wire under a cap of 0, because group 1 was taken while it was still
		// in range. Nothing raises the cap to rescue it, so the assertion stays sensitive to
		// the window under test: had the prefetch not taken group 1, the cap would hold it in
		// the buffer and the second read would go idle instead.
		expect(await sub.servedSequence()).toBe(0);
		expect(await sub.servedSequence()).toBe(1);
	} finally {
		await sub.close();
	}
});

// The other end of the subscription is not symmetric. Raising the start floor is destructive
// by design, since the read cursor shifts and closes every buffered group below it, so a group
// already in hand has to go the same way: serving it would re-deliver below a floor the
// subscriber just moved, which on a route splice is duplicate media.
test("lite draft-06: a group taken before the floor rose past it is dropped", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0, gated: true });
	try {
		// Same window as the cap test: group 0 parks the loop, so the prefetch takes group 1.
		sub.serve(0);
		await sub.parked;
		sub.serve(1);
		await flush();

		// Skip ahead past group 1, which the loop is already holding.
		await new SubscribeUpdate({ priority: 0, startGroup: 2 }).encode(sub.client.writer, Version.DRAFT_06);
		while (sub.track.subscription.peek()?.startGroup !== 2) await sub.track.subscription.changed();

		// Buffered while the loop is still parked, so it is taken under the raised floor.
		sub.serve(2);
		sub.release();

		// Group 0 was already decided and stays in flight; group 1 must not follow it.
		expect(await sub.servedSequence()).toBe(0);
		expect(await sub.servedSequence()).toBe(2);
	} finally {
		await sub.close();
	}
});

// Raising the floor into a group already in hand is just as destructive as raising it past
// the group. The group stays servable, but its snapshotted frame range must be lifted to the
// new floor so frames the subscriber discarded do not reach the wire.
test("lite draft-06: a group taken before the floor rose into it starts at the raised frame", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0,
		frames: ["a", "b", "c"],
		gated: true,
	});
	try {
		// Group 0 parks the loop, so the armed prefetch takes group 1 under the old floor.
		sub.serve(0);
		await sub.parked;
		sub.serve(1);
		await flush();

		// Raise the floor into held group 1, excluding its first two frames.
		await new SubscribeUpdate({ priority: 0, startGroup: 1, startFrame: 2 }).encode(
			sub.client.writer,
			Version.DRAFT_06,
		);
		while (sub.track.subscription.peek()?.startGroup !== 1) await sub.track.subscription.changed();

		sub.release();

		// Group 0 was already decided and stays in flight. Group 1 must honor the newer,
		// destructive floor even though it was taken with a start frame of 0.
		expect(await sub.servedGroup()).toEqual({ sequence: 0, frameStart: 0, payloads: ["a", "b", "c"] });
		expect(await sub.servedGroup()).toEqual({ sequence: 1, frameStart: 2, payloads: ["c"] });
	} finally {
		await sub.close();
	}
});

// Frame bounds have to travel with the group they were taken under. An update that moves the
// cap off a group already in hand leaves it matching neither boundary, and mapping that to
// the whole group would put frames the subscription excluded on the wire. Nothing downstream
// trims them: a frame range is a wire request, not a receiver-side cursor.
test("lite draft-06: a prefetched group keeps the frame bounds it was taken under", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0,
		endGroup: 1,
		endFrame: 1,
		frames: ["a", "b", "c"],
		gated: true,
	});
	try {
		// Same window as above: group 0 parks the loop, so the armed prefetch takes group 1.
		sub.serve(0);
		await sub.parked;
		sub.serve(1);
		await flush();

		// Move the cap off group 1, which the loop is already holding.
		await new SubscribeUpdate({ priority: 0, endGroup: 0 }).encode(sub.client.writer, Version.DRAFT_06);
		while (sub.track.subscription.peek()?.endGroup !== 0) await sub.track.subscription.changed();

		sub.release();

		// Group 0 was never the end group, so it goes whole either way.
		expect(await sub.servedGroup()).toEqual({ sequence: 0, frameStart: 0, payloads: ["a", "b", "c"] });
		// Group 1 was taken while it was the end group, capped at frame 1, so "c" stays off
		// the wire even though the cap has since moved below it.
		expect(await sub.servedGroup()).toEqual({ sequence: 1, frameStart: 0, payloads: ["a", "b"] });
	} finally {
		await sub.close();
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
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomOrigin(), origin.consume());

	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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

	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin(), origin.consume());
	const broadcast = origin.publish(Path.from("test"));
	const track = broadcast.createTrack("video");

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
