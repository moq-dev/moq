import { expect, test } from "bun:test";
import * as broadcast from "../broadcast.ts";
import { HopSchema, randomHop, UNKNOWN_HOP } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { wireOf } from "../wire.ts";
import { Group as GroupMessage } from "./group.ts";
import { StreamId } from "./stream.ts";
import { encodeSubscribeResponse, Subscribe, SubscribeStart } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { ALPN_07_WIP, Version } from "./version.ts";

const VERSION = Version.DRAFT_07;

/** Whether `promise` settles within `ms`. */
async function settlesWithin(promise: Promise<unknown>, ms: number): Promise<boolean> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const pending = new Promise<false>((resolve) => {
		timer = setTimeout(() => resolve(false), ms);
	});
	try {
		return await Promise.race([promise.then(() => true), pending]);
	} finally {
		clearTimeout(timer);
	}
}

test("a broadcast originating here names one random origin for its life", () => {
	const producer = new broadcast.Producer();
	const origin = wireOf(producer).origin();
	expect(origin).not.toBe(UNKNOWN_HOP);
	expect(wireOf(producer).origin()).toBe(origin);
	// Every handle, and so every session serving it, names the same one.
	expect(wireOf(producer.consume()).origin()).toBe(origin);
	expect(wireOf(new broadcast.Producer()).origin()).not.toBe(origin);
});

test("a draft-07 group waits for SUBSCRIBE_START, whose origin the broadcast then names", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const subscriber = new Subscriber(pair.client, VERSION, randomHop());
	const consumer = subscriber.consume(Path.from("room"));
	const reader = consumer.track("video").subscribe({});

	const info = await Stream.accept(pair.server);
	if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
	expect(await info.reader.u53()).toBe(StreamId.Track);
	await TrackMessage.decode(info.reader, VERSION);
	await new TrackInfo({ maxAge: 60_000 }).encode(info.writer, VERSION);
	info.close();

	const sub = await Stream.accept(pair.server);
	if (!sub) throw new Error("the subscriber never subscribed");
	expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(sub.reader, VERSION);

	// The group's stream races ahead of the subscribe stream.
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	void subscriber.runGroup(new GroupMessage({ subscribe: 0n, sequence: 0 }), new Reader(readable));
	controller.enqueue(new Uint8Array([0, 1, 120]));
	controller.close();

	const next = reader.recvGroup();
	expect(await settlesWithin(next, 50)).toBe(false);

	const origin = HopSchema.parse(42n);
	await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0, origin) }, VERSION);
	const group = await next;
	expect(group?.sequence).toBe(0);
	expect(await group?.readString()).toBe("x");

	// Republishing the broadcast proxies the origin upstream named.
	expect(wireOf(consumer).origin()).toBe(origin);
});
