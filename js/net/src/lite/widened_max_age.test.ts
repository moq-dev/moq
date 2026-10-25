// A subscriber widening its max age over the wire gets the cached groups the publisher skipped
// as stale, including those below where SUBSCRIBE_START resolved, and never one twice.

import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { accept, connect } from "../connection/index.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Subscriber as TrackSubscriber } from "../track.ts";
import { wireOf } from "../wire.ts";
import { ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");

/** Every group `reader` hands out until it goes quiet, and the read still waiting. */
async function received(
	reader: TrackSubscriber,
	waiting?: Promise<GroupConsumer | undefined>,
): Promise<{ got: number[]; waiting: Promise<GroupConsumer | undefined> }> {
	const got: number[] = [];
	for (;;) {
		const next = waiting ?? reader.recvGroup();
		waiting = next;
		const quiet = new Promise<"quiet">((resolve) => setTimeout(() => resolve("quiet"), 100));
		const group = await Promise.race([next, quiet]);
		if (group === "quiet") return { got: got.sort((a, b) => a - b), waiting: next };
		waiting = undefined;
		if (!group) throw new Error("the track ended");
		got.push(group.sequence);
	}
}

test("a widened maxDelay re-offers what the publisher skipped below the start", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const broadcast = origin.createBroadcast(Path.from("room"));
	broadcast.announce();
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// One group per second, so a budget picks how far back a reader reaches.
	for (let sequence = 0; sequence < 10; sequence++) {
		const group = producer.appendGroup();
		group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Timestamp.fromMillis(sequence * 1000) });
		group.close();
	}

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: Version.DRAFT_07, publish: origin.consume() }),
	]);
	const remote = wireOf(client).consume(Path.from("room"));
	// Group 5 ends at 6s, a full 3s behind the 9s edge.
	const reader = remote.track("video").subscribe({ maxDelay: Milli(2500) });
	const first = await received(reader);
	expect(first.got).toEqual([6, 7, 8, 9]);

	reader.update({ maxDelay: Milli(6500) });
	const widened = await received(reader, first.waiting);
	expect(widened.got).toEqual([2, 3, 4, 5]);

	reader.close();
	remote.close();
	broadcast.close();
	origin.close();
	client.abort();
	server.abort();
});
