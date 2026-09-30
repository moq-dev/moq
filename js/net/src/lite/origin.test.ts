import { expect, test } from "bun:test";
import { Producer as GroupProducer } from "../group.ts";
import { type Hop, HopSchema, randomHop } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Fetch, FetchOk } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Publisher } from "./publisher.ts";
import { StreamId } from "./stream.ts";
import { decodeSubscribeResponse, encodeSubscribeResponse, Subscribe, SubscribeStart } from "./subscribe.ts";
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

// JS publishes only what it produces, so the session is always the origin and names its own
// hop, as Rust names its origin's.
test("a draft-07 publisher names its own hop in SUBSCRIBE_START and FETCH_OK", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const hop = randomHop();
	const publisher = new Publisher(pair.server, VERSION, hop, origin.consume());
	const produced = origin.createBroadcast(Path.from("room"));
	produced.announce();
	const track = produced.createTrack("video");
	const group = new GroupProducer(0);
	group.writeString("x");
	group.close();
	track.writeGroup(group);

	const sub = await Stream.open(pair.client, { version: VERSION });
	const serving = await Stream.accept(pair.server, VERSION);
	if (!serving) throw new Error("the publisher never accepted the subscribe stream");
	void publisher.runSubscribe(
		new Subscribe({ id: 0n, broadcast: Path.from("room"), track: "video", priority: 0 }),
		serving,
	);
	const start = await decodeSubscribeResponse(sub.reader, VERSION);
	if (!("start" in start)) throw new Error("expected SUBSCRIBE_START");
	expect(start.start.origin).toBe(hop);

	const fetch = await Stream.open(pair.client, { version: VERSION });
	const fetching = await Stream.accept(pair.server, VERSION);
	if (!fetching) throw new Error("the publisher never accepted the fetch stream");
	void publisher.runFetch(
		new Fetch({ broadcast: Path.from("room"), track: "video", priority: 0, group: 0 }),
		fetching,
	);
	expect((await FetchOk.decode(fetch.reader, VERSION)).origin).toBe(hop);

	publisher.close();
	sub.close();
	fetch.close();
});

// Plays the upstream publisher on a mock draft-07 session, answering the subscriber's streams.
class Upstream {
	readonly pair = createMockTransportPair(ALPN_07_WIP);
	readonly subscriber = new Subscriber(this.pair.client, VERSION, randomHop());

	// Answer a TRACK stream, then accept and start the SUBSCRIBE that follows it at group 0,
	// naming `origin`.
	async subscribe(origin: Hop): Promise<Stream> {
		const info = await Stream.accept(this.pair.server, VERSION);
		if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
		expect(await info.reader.u53()).toBe(StreamId.Track);
		await TrackMessage.decode(info.reader, VERSION);
		await new TrackInfo({ maxAge: 60_000 }).encode(info.writer, VERSION);
		info.close();

		const sub = await Stream.accept(this.pair.server, VERSION);
		if (!sub) throw new Error("the subscriber never subscribed");
		expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
		await Subscribe.decode(sub.reader, VERSION);
		await this.start(sub, origin);
		return sub;
	}

	async start(sub: Stream, origin: Hop): Promise<void> {
		await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0, origin) }, VERSION);
	}
}

// JS used to keep one origin per broadcast, which the latest reply overwrote. Each track copy
// now keeps its own, so two tracks served by different origins are both fine.
test("tracks of one broadcast keep the origins their own replies named", async () => {
	const upstream = new Upstream();
	const consumer = upstream.subscriber.consume(Path.from("room"));

	const video = consumer.track("video").subscribe({});
	const videoSub = await upstream.subscribe(HopSchema.parse(42n));
	const audio = consumer.track("audio").subscribe({});
	await upstream.subscribe(HopSchema.parse(43n));
	// Repeating a copy's own origin is not a change.
	await upstream.start(videoSub, HopSchema.parse(42n));

	expect(await settlesWithin(Promise.race([video.closed, audio.closed]), 50)).toBe(false);

	upstream.subscriber.close();
});

// Mirrors Rust's track::Provenance: a copy has one origin, so a reply naming another means
// upstream is serving a different track. The copy is dropped rather than relabeled.
test("a SUBSCRIBE_START naming a second origin drops the copy", async () => {
	const upstream = new Upstream();
	const consumer = upstream.subscriber.consume(Path.from("room"));

	const reader = consumer.track("video").subscribe({});
	const sub = await upstream.subscribe(HopSchema.parse(42n));
	await upstream.start(sub, HopSchema.parse(43n));

	const closed = await reader.closed;
	expect(closed).toBeInstanceOf(Error);
	expect((closed as Error).message).toContain("origin changed");

	upstream.subscriber.close();
});

test("a draft-07 group waits for SUBSCRIBE_START", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const subscriber = new Subscriber(pair.client, VERSION, randomHop());
	const consumer = subscriber.consume(Path.from("room"));
	const reader = consumer.track("video").subscribe({});

	const info = await Stream.accept(pair.server, VERSION);
	if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
	expect(await info.reader.u53()).toBe(StreamId.Track);
	await TrackMessage.decode(info.reader, VERSION);
	await new TrackInfo({ maxAge: 60_000 }).encode(info.writer, VERSION);
	info.close();

	const sub = await Stream.accept(pair.server, VERSION);
	if (!sub) throw new Error("the subscriber never subscribed");
	expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(sub.reader, VERSION);

	// The group's stream races ahead of the subscribe stream.
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	void subscriber.runGroup(
		new GroupMessage({ subscribe: 0n, sequence: 0 }),
		new Reader(readable, undefined, VERSION),
	);
	controller.enqueue(new Uint8Array([0, 1, 120]));
	controller.close();

	const next = reader.recvGroup();
	expect(await settlesWithin(next, 50)).toBe(false);

	await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0, HopSchema.parse(42n)) }, VERSION);
	const group = await next;
	expect(group?.sequence).toBe(0);
	expect(await group?.readString()).toBe("x");

	subscriber.close();
});
