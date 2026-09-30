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
import {
	decodeSubscribeResponse,
	encodeSubscribeResponse,
	Subscribe,
	SubscribeEnd,
	SubscribeStart,
} from "./subscribe.ts";
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

// Serve one SUBSCRIBE and one FETCH of `track` over a fresh draft-07 session publishing `origin`,
// returning the origins its SUBSCRIBE_START and FETCH_OK name.
async function servedOrigins(origin: OriginProducer, track: string): Promise<[Hop, Hop]> {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const publisher = new Publisher(pair.server, VERSION, randomHop(), origin.consume());

	const sub = await Stream.open(pair.client, { version: VERSION });
	const serving = await Stream.accept(pair.server, VERSION);
	if (!serving) throw new Error("the publisher never accepted the subscribe stream");
	void publisher.runSubscribe(new Subscribe({ id: 0n, broadcast: Path.from("room"), track, priority: 0 }), serving);
	const start = await decodeSubscribeResponse(sub.reader, VERSION);
	if (!("start" in start)) throw new Error("expected SUBSCRIBE_START");

	const fetch = await Stream.open(pair.client, { version: VERSION });
	const fetching = await Stream.accept(pair.server, VERSION);
	if (!fetching) throw new Error("the publisher never accepted the fetch stream");
	void publisher.runFetch(new Fetch({ broadcast: Path.from("room"), track, priority: 0, group: 0 }), fetching);
	const ok = await FetchOk.decode(fetch.reader, VERSION);

	publisher.close();
	sub.close();
	fetch.close();
	return [start.start.origin, ok.origin];
}

// JS publishes only what it produces, so the origin it names is its own: one identity per
// origin, shared by every session serving it (Rust's `origin.hop()`). A relay failing over
// from one session to another sees the same origin and resumes rather than ending the broadcast.
test("every draft-07 session serving an origin names the same origin", async () => {
	const origin = new OriginProducer();
	const produced = origin.createBroadcast(Path.from("room"));
	produced.announce();
	const track = produced.createTrack("video");
	const group = new GroupProducer(0);
	group.writeString("x");
	group.close();
	track.writeGroup(group);

	const [start, fetched] = await servedOrigins(origin, "video");
	expect(fetched).toBe(start);
	expect(await servedOrigins(origin, "video")).toEqual([start, start]);

	const other = new OriginProducer();
	const elsewhere = other.createBroadcast(Path.from("room"));
	elsewhere.announce();
	elsewhere.createTrack("video").writeGroup(group);
	expect((await servedOrigins(other, "video"))[0]).not.toBe(start);
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

	// End a started subscription cleanly with no groups served.
	async end(sub: Stream): Promise<void> {
		await encodeSubscribeResponse(sub.writer, { end: new SubscribeEnd(0, 0) }, VERSION);
		sub.close();
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
	const audioSub = await upstream.subscribe(HopSchema.parse(43n));
	// Repeating a copy's own origin is not a change.
	await upstream.start(videoSub, HopSchema.parse(42n));

	// Both end cleanly, after every START was read, rather than with a mismatch.
	await upstream.end(videoSub);
	await upstream.end(audioSub);
	expect(await video.closed).toBeNull();
	expect(await audio.closed).toBeNull();

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
