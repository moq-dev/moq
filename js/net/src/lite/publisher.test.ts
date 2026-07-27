import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { Producer as GroupProducer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Group as GroupMessage } from "./group.ts";
import { randomOrigin } from "./origin.ts";
import { Publisher } from "./publisher.ts";
import { decodeSubscribeResponse, Subscribe } from "./subscribe.ts";
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
 * reports every group stream that reached the wire plus the resolved SUBSCRIBE_START.
 */
async function serve(
	groups: Record<number, string[]>,
	bounds: { startGroup?: number; startFrame?: number; endGroup?: number; endFrame?: number },
): Promise<{ start?: number; served: Served[] }> {
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
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_06);
			if ("start" in resp) start = resp.start.group;
			if ("end" in resp) break;
		}
		return { start, served };
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
