// A subscriber widening its max age over the wire gets the cached groups the publisher skipped
// as stale, including those below where SUBSCRIBE_START resolved, and never one twice.

import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { accept, connect } from "../connection/index.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Subscriber as TrackSubscriber } from "../track.ts";
import { wireOf } from "../wire.ts";
import { ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");

/** The next `count` groups `reader` hands out, in sequence order. */
async function received(reader: TrackSubscriber, count: number): Promise<bigint[]> {
	const got: bigint[] = [];
	while (got.length < count) {
		const group = await reader.recvGroup();
		if (!group) throw new Error("the track ended");
		got.push(group.sequence);
	}
	return got.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}

test("a widened maxDelay re-offers what the publisher skipped below the start", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const broadcast = publication.broadcast;
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// One group per second, so a budget picks how far back a reader reaches.
	for (let sequence = 0; sequence < 10; sequence++) {
		const group = producer.appendGroup();
		group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Timestamp.fromMillis(sequence * 1000) });
		group.finish();
	}

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: Version.DRAFT_07, publish: origin.consume() }),
	]);
	const remote = wireOf(client).consume(Path.from("room"));
	// Group 5 ends at 6s, a full 3s behind the 9s edge.
	const reader = remote.track("video").subscribe({ delay: { max: Milli(2500) } });
	expect(await received(reader, 4)).toEqual([6n, 7n, 8n, 9n]);

	reader.update({ delay: { max: Milli(6500) } });
	expect(await received(reader, 4)).toEqual([2n, 3n, 4n, 5n]);
	// Ending the track flushes anything else the publisher would send: nothing, not even a repeat.
	producer.finish();
	producer.close();
	expect(await reader.recvGroup()).toBeUndefined();

	reader.close();
	remote.close();
	broadcast.close();
	origin.close();
	client.abort();
	server.abort();
});

for (const [version, alpn] of [
	[Version.DRAFT_06, ALPN_06],
	[Version.DRAFT_07, ALPN_07_WIP],
] as const) {
	const reoffers = version === Version.DRAFT_07;
	test(`a widened maxDelay ${reoffers ? "re-offers" : "keeps dropped"} the groups reported stale on ${alpn}`, async () => {
		const pair = createMockTransportPair(alpn);
		const origin = new OriginProducer();
		const publication = origin.createBroadcast(Path.from("room"));
		publication.announce();
		const broadcast = publication.broadcast;
		const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const write = (sequence: number, millis: number) => {
			const group = producer.createGroup(BigInt(sequence));
			group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Timestamp.fromMillis(millis) });
			group.finish();
		};
		write(0, 0);

		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ transport: pair.server, url, version, publish: origin.consume() }),
		]);
		const remote = wireOf(client).consume(Path.from("room"));
		const reader = remote.track("video").subscribe({ delay: { max: Milli(1500) } });
		expect(await received(reader, 1)).toEqual([0n]);
		// Groups 1 and 2 reach 2s and 3s, at least 1.5s behind the 6s edge, so they are reported
		// stale after SUBSCRIBE_START.
		for (const [sequence, millis] of [
			[1, 1000],
			[2, 2000],
			[3, 3000],
			[4, 6000],
		]) {
			write(sequence, millis);
		}
		expect(await received(reader, 2)).toEqual([3n, 4n]);

		// Lite-07 re-offers only the groups reported stale; earlier subscribers settled them for good.
		reader.update({ delay: { max: Milli(10_000) } });
		if (reoffers) expect(await received(reader, 2)).toEqual([1n, 2n]);
		producer.finish();
		producer.close();
		expect(await reader.recvGroup()).toBeUndefined();

		reader.close();
		remote.close();
		broadcast.close();
		origin.close();
		client.abort();
		server.abort();
	});
}

test("a widening keeps a bounded lite-07 subscription open for the stale groups it re-offers", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const broadcast = publication.broadcast;
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const write = (sequence: number, millis: number) => {
		const group = producer.createGroup(BigInt(sequence));
		group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Timestamp.fromMillis(millis) });
		group.finish();
	};
	write(0, 0);

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: Version.DRAFT_07, publish: origin.consume() }),
	]);
	const remote = wireOf(client).consume(Path.from("room"));
	const ranges = [{ start: { absolute: 0n }, end: { absolute: 5n } }];
	const reader = remote.track("video").subscribe({ delay: { max: Milli(1500) }, ranges });
	expect(await received(reader, 1)).toEqual([0n]);
	// Groups 1 and 2 are reported stale after SUBSCRIBE_START.
	for (const [sequence, millis] of [
		[1, 1000],
		[2, 2000],
		[3, 3000],
		[4, 6000],
	]) {
		write(sequence, millis);
	}
	expect(await received(reader, 2)).toEqual([3n, 4n]);

	// The widening re-offers them, and the track's end does not settle the tail before they arrive.
	reader.update({ delay: { max: Milli(10_000) }, ranges });
	producer.finish();
	expect(await received(reader, 2)).toEqual([1n, 2n]);

	reader.close();
	remote.close();
	broadcast.close();
	origin.close();
	client.abort();
	server.abort();
});

// Every lite-07 update reopens the stale drops, widening or not: the publisher drops again, under
// the new update's number, what is still stale, so the subscriber settles at once rather than
// waiting out its grace for a re-offer that never comes.
test("an update keeping the budget drops the stale groups again on lite-07", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const broadcast = publication.broadcast;
	const producer = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const write = (sequence: number, millis: number) => {
		const group = producer.createGroup(BigInt(sequence));
		group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Timestamp.fromMillis(millis) });
		group.finish();
	};
	write(0, 0);

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: Version.DRAFT_07, publish: origin.consume() }),
	]);
	const remote = wireOf(client).consume(Path.from("room"));
	const reader = remote.track("video").subscribe({ delay: { max: Milli(1500) } });
	expect(await received(reader, 1)).toEqual([0n]);
	for (const [sequence, millis] of [
		[1, 1000],
		[2, 2000],
		[3, 3000],
		[4, 6000],
	]) {
		write(sequence, millis);
	}
	expect(await received(reader, 2)).toEqual([3n, 4n]);

	// Only the priority changes: groups 1 and 2 are still stale, so they are dropped again.
	reader.update({ priority: 5 });
	const before = performance.now();
	producer.finish();
	producer.close();
	expect(await reader.recvGroup()).toBeUndefined();
	expect(performance.now() - before).toBeLessThan(500);

	reader.close();
	remote.close();
	broadcast.close();
	origin.close();
	client.abort();
	server.abort();
});
