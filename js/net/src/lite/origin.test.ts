import { expect, test } from "bun:test";
import * as broadcast from "../broadcast.ts";
import { type Hop, HopSchema, randomHop, UNKNOWN_HOP } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { wireOf } from "../wire.ts";
import { Fetch, FetchOk } from "./fetch.ts";
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

// The origin a republishing session names for the track it serves off `from`.
function served(from: broadcast.Producer | broadcast.Consumer, name: string): Hop {
	return wireOf(from).origin(wireOf(from).subscribe(name));
}

test("a broadcast originating here names one random origin for its life", () => {
	const producer = new broadcast.Producer();
	producer.createTrack("video");
	producer.createTrack("audio");

	const origin = served(producer, "video");
	expect(origin).not.toBe(UNKNOWN_HOP);
	expect(served(producer, "video")).toBe(origin);
	// Every track and every handle, and so every session serving it, names the same one.
	expect(served(producer, "audio")).toBe(origin);
	expect(served(producer.consume(), "video")).toBe(origin);

	const other = new broadcast.Producer();
	other.createTrack("video");
	expect(served(other, "video")).not.toBe(origin);
});

// Plays the upstream publisher on a mock draft-07 session, answering the subscriber's streams.
class Upstream {
	readonly pair = createMockTransportPair(ALPN_07_WIP);
	readonly subscriber = new Subscriber(this.pair.client, VERSION, randomHop());

	// Answer a TRACK stream, then accept the SUBSCRIBE or FETCH stream that follows it.
	async accept(): Promise<Stream> {
		const info = await Stream.accept(this.pair.server);
		if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
		expect(await info.reader.u53()).toBe(StreamId.Track);
		await TrackMessage.decode(info.reader, VERSION);
		await new TrackInfo({ maxAge: 60_000 }).encode(info.writer, VERSION);
		info.close();

		const stream = await Stream.accept(this.pair.server);
		if (!stream) throw new Error("the subscriber never requested the track");
		return stream;
	}

	// Accept a SUBSCRIBE and start it at group 0, naming `origin`.
	async subscribe(origin: Hop): Promise<Stream> {
		const sub = await this.accept();
		expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
		await Subscribe.decode(sub.reader, VERSION);
		await this.start(sub, origin);
		return sub;
	}

	async start(sub: Stream, origin: Hop): Promise<void> {
		await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0, origin) }, VERSION);
	}

	// Accept a FETCH and answer it with an empty group, naming `origin`.
	async fetch(origin: Hop): Promise<void> {
		const stream = await this.accept();
		expect(await stream.reader.u53()).toBe(StreamId.Fetch);
		await Fetch.decode(stream.reader, VERSION);
		await new FetchOk(origin).encode(stream.writer, VERSION);
		stream.close();
	}
}

// Resolves once `check` holds, polling across macrotasks for the subscriber to read a reply.
async function until(check: () => boolean): Promise<void> {
	for (let i = 0; i < 100 && !check(); i++) await new Promise((resolve) => setTimeout(resolve, 1));
	expect(check()).toBe(true);
}

// JS used to keep one origin per broadcast, so the latest reply relabeled content an earlier
// one named. Each track copy and each fetched group now keeps the origin its own reply named.
test("tracks and fetched groups of one broadcast keep the origins their own replies named", async () => {
	const upstream = new Upstream();
	const consumer = upstream.subscriber.consume(Path.from("room"));
	const video = HopSchema.parse(42n);
	const audio = HopSchema.parse(43n);
	const archive = HopSchema.parse(44n);

	// A republishing session serves a subscription of its own off each cached copy.
	const videoReader = consumer.track("video").subscribe({});
	await upstream.subscribe(video);
	await until(() => served(consumer, "video") === video);
	const audioReader = consumer.track("audio").subscribe({});
	await upstream.subscribe(audio);
	await until(() => served(consumer, "audio") === audio);

	const fetched = consumer.track("video").fetchGroup(0);
	await upstream.fetch(archive);
	const group = await fetched;

	expect(wireOf(consumer).origin(group)).toBe(archive);
	expect(served(consumer, "video")).toBe(video);
	expect(served(consumer, "audio")).toBe(audio);
	expect(videoReader.closed.peek()).toBeUndefined();
	expect(audioReader.closed.peek()).toBeUndefined();

	upstream.subscriber.close();
});

// Mirrors Rust's track::Provenance: a copy has one origin, so a reply naming another means
// upstream is serving a different track. The copy is dropped rather than relabeled.
test("a SUBSCRIBE_START naming a second origin drops the copy", async () => {
	const upstream = new Upstream();
	const consumer = upstream.subscriber.consume(Path.from("room"));
	const origin = HopSchema.parse(42n);

	const reader = consumer.track("video").subscribe({});
	const sub = await upstream.subscribe(origin);
	await until(() => served(consumer, "video") === origin);

	await upstream.start(sub, HopSchema.parse(43n));
	const closed = await reader.closed;
	expect(closed).toBeInstanceOf(Error);
	expect((closed as Error).message).toContain("origin changed");

	upstream.subscriber.close();
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

	// Republishing the track proxies the origin upstream named.
	expect(wireOf(consumer).origin(reader)).toBe(origin);
});
