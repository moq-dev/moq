import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { Producer as GroupProducer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { Fetch } from "./fetch.ts";
import { randomOrigin } from "./origin.ts";
import { sendOrder } from "./priority.ts";
import { Publisher } from "./publisher.ts";
import { decodeSubscribeResponse, Subscribe, SubscribeUpdate } from "./subscribe.ts";
import { ALPN_05, Version } from "./version.ts";

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
		// order given rather than whatever the cache hands over in one batch.
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	track.close();

	try {
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_05);
			if ("end" in resp) return resp.end.group;
		}
	} finally {
		broadcast.close();
		client.close();
	}
}

// Serves `sequences` at the subscribed priority, optionally raising it via SUBSCRIBE_UPDATE
// after the first group, and returns the send order of each group stream in open order.
async function groupSendOrders(priority: number, sequences: number[], update?: number) {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority });
	void publisher.runSubscribe(msg, server);

	try {
		for (const [index, sequence] of sequences.entries()) {
			if (index === 1 && update !== undefined) {
				await new SubscribeUpdate({ priority: update }).encode(client.writer, Version.DRAFT_05);
				// Let the publisher apply it, otherwise the next group races the update on the wire.
				await new Promise((resolve) => setTimeout(resolve, 5));
			}

			const group = new GroupProducer(sequence);
			group.writeString("hello");
			group.close();
			track.writeGroup(group);

			// One group per turn, so the send orders come back in the order given.
			await new Promise((resolve) => setTimeout(resolve, 5));
		}
		track.close();

		// The last group's stream opens asynchronously, so wait for it rather than racing it.
		for (let i = 0; i < 200 && pair.server.sendStreams.uni.length < sequences.length; i++) {
			await new Promise((resolve) => setTimeout(resolve, 5));
		}

		return pair.server.sendStreams.uni.map((stream) => stream.sendOrder);
	} finally {
		broadcast.close();
		client.close();
	}
}

// Without a send order every group stream ranks the same, so the transport round-robins them
// and a stalled low-priority track steals bandwidth from the one the subscriber asked for.
test("lite draft-05: group streams carry the subscription's send order", async () => {
	expect(await groupSendOrders(7, [0, 1, 2])).toEqual([sendOrder(7, 0), sendOrder(7, 1), sendOrder(7, 2)]);
});

// SUBSCRIBE_UPDATE re-ranks the subscription, so groups opened after it use the new priority.
test("lite draft-05: a subscribe update re-ranks later groups", async () => {
	const orders = await groupSendOrders(1, [0, 1], 9);
	expect(orders).toEqual([sendOrder(1, 0), sendOrder(9, 1)]);
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

	for (let i = 0; i < 200 && pair.server.sendStreams.uni.length < 1; i++) {
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	const stream = pair.server.sendStreams.uni[0];
	expect(stream.sendOrder).toBe(sendOrder(1, 4));

	await new SubscribeUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
	for (let i = 0; i < 200 && stream.sendOrder === sendOrder(1, 4); i++) {
		await new Promise((resolve) => setTimeout(resolve, 5));
	}

	expect(stream.sendOrder).toBe(sendOrder(9, 4));

	group.close();
	track.close();
	broadcast.close();
	client.close();
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

	const msg = new Fetch(Path.from("test"), "video", 3, 7);
	await publisher.runFetch(msg, server);

	expect((accepted.value.writable as { sendOrder?: number }).sendOrder).toBe(sendOrder(3, 7));

	broadcast.close();
	client.close();
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
