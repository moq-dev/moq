// An open Track Stream is interest in the track: the subscriber keeps it open until its
// SUBSCRIBE is answered, and the publisher keeps the track wanted until the subscriber closes it.
import { describe, expect, test } from "bun:test";
import { randomHop } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { Milli, Timescale } from "../time.ts";
import { wireOf } from "../wire.ts";
import { Fetch } from "./fetch.ts";
import { Publisher } from "./publisher.ts";
import { StreamId } from "./stream.ts";
import { encodeSubscribeResponse, Subscribe, SubscribeStart } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { ALPN_05, Version } from "./version.ts";

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

describe.each([Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07])("%s", (version) => {
	/** A subscriber whose publisher the test plays by hand, up to the SUBSCRIBE. */
	async function subscribing() {
		const pair = createMockTransportPair(ALPN_05);
		const subscriber = new Subscriber(pair.client, version, randomHop());
		const reader = subscriber
			.consume(Path.from("room"))
			.track("video")
			.subscribe({ maxDelay: Milli(100) });

		const track = await Stream.accept(pair.server, version);
		if (!track) throw new Error("the subscriber never asked for TRACK_INFO");
		expect(await track.reader.u53()).toBe(StreamId.Track);
		await TrackMessage.decode(track.reader, version);
		await new TrackInfo({}).encode(track.writer, version);
		track.writer.close();

		const sub = await Stream.accept(pair.server, version);
		if (!sub) throw new Error("the subscriber never subscribed");
		expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
		await Subscribe.decode(sub.reader, version);
		return { subscriber, reader, track, sub };
	}

	test("a subscription holds its TRACK stream until the SUBSCRIBE is answered", async () => {
		const { subscriber, reader, track, sub } = await subscribing();
		const fin = track.reader.done();
		expect(await settlesWithin(fin, 50)).toBe(false);

		await encodeSubscribeResponse(sub.writer, { start: new SubscribeStart(0) }, version);
		expect(await fin).toBe(true);

		reader.close();
		subscriber.close();
	});

	test("a subscription that ends unanswered closes its TRACK stream", async () => {
		const { subscriber, reader, track } = await subscribing();
		const fin = track.reader.done();
		expect(await settlesWithin(fin, 50)).toBe(false);

		reader.close();
		expect(await fin).toBe(true);
		subscriber.close();
	});

	test("an info query closes its TRACK stream once answered", async () => {
		const pair = createMockTransportPair(ALPN_05);
		const subscriber = new Subscriber(pair.client, version, randomHop());
		const info = subscriber.consume(Path.from("room")).track("video").info();

		const track = await Stream.accept(pair.server, version);
		if (!track) throw new Error("the subscriber never asked for TRACK_INFO");
		expect(await track.reader.u53()).toBe(StreamId.Track);
		await TrackMessage.decode(track.reader, version);
		await new TrackInfo({ priority: 3 }).encode(track.writer, version);
		track.writer.close();

		expect((await info).priority).toBe(3);
		expect(await track.reader.done()).toBe(true);
		subscriber.close();
	});

	test("a held info lookup abandoned before TRACK_INFO resets its TRACK stream", async () => {
		const pair = createMockTransportPair(ALPN_05);
		const subscriber = new Subscriber(pair.client, version, randomHop());
		const hold = new AbortController();
		const info = subscriber.resolveTrackInfo(Path.from("room"), "video", undefined, hold.signal);

		const track = await Stream.accept(pair.server, version);
		if (!track) throw new Error("the subscriber never asked for TRACK_INFO");
		expect(await track.reader.u53()).toBe(StreamId.Track);
		await TrackMessage.decode(track.reader, version);

		hold.abort();
		await expect(info).rejects.toThrow();
		await expect(track.reader.done()).rejects.toThrow();
		subscriber.close();
	});

	test.each(["FIN", "reset"] as const)(
		"an answered TRACK stream keeps the track wanted until the requester's %s",
		async (close) => {
			const pair = createMockTransportPair(ALPN_05);
			const origin = new OriginProducer();
			const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
			const broadcast = origin.createBroadcast(Path.from("room"));
			broadcast.announce();
			const demand = broadcast.demand();
			const serving = (async () => {
				for (;;) {
					const request = await wireOf(broadcast).requested();
					if (!request) return;
					request.accept({ timescale: Timescale.MILLI });
				}
			})();

			const client = await Stream.open(pair.client, { version });
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("the publisher never accepted the TRACK stream");
			void publisher.runTrackInfo(new TrackMessage(Path.from("room"), "video"), server);

			await TrackInfo.decode(client.reader, version);
			expect(await client.reader.done()).toBe(true);
			expect(await settlesWithin(demand.unused(), 50)).toBe(false);

			if (close === "FIN") client.writer.close();
			else client.writer.reset(new Error("gone"));
			expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

			publisher.close();
			broadcast.close();
			await serving;
			origin.close();
		},
	);

	test.each(["FIN", "reset"] as const)(
		"a requester's %s while TRACK_INFO is blocked lets the track go",
		async (close) => {
			const pair = createMockTransportPair(ALPN_05);
			const origin = new OriginProducer();
			const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
			const broadcast = origin.createBroadcast(Path.from("room"));
			broadcast.announce();
			const demand = broadcast.demand();
			const serving = (async () => {
				for (;;) {
					const request = await wireOf(broadcast).requested();
					if (!request) return;
					request.accept({ timescale: Timescale.MILLI });
				}
			})();

			const client = await Stream.open(pair.client, { version });
			const incoming = pair.server.incomingBidirectionalStreams.getReader();
			const accepted = await incoming.read();
			incoming.releaseLock();
			if (accepted.done) throw new Error("the publisher never saw the TRACK stream");
			// A send side that never gets credit, so the reply stays blocked.
			let blocked!: () => void;
			const writing = new Promise<void>((resolve) => (blocked = resolve));
			const writable = new WritableStream<Uint8Array>({
				write: () => {
					blocked();
					return new Promise<void>(() => {});
				},
			});
			const server = new Stream({ readable: accepted.value.readable, writable, version });
			void publisher.runTrackInfo(new TrackMessage(Path.from("room"), "video"), server);

			await writing;
			expect(demand.used.peek()).toBe(true);

			if (close === "FIN") client.writer.close();
			else client.writer.reset(new Error("gone"));
			expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

			publisher.close();
			broadcast.close();
			await serving;
			origin.close();
		},
	);

	test("one TRACK stream closing leaves the track wanted while another holds it", async () => {
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const broadcast = origin.createBroadcast(Path.from("room"));
		broadcast.announce();
		const demand = broadcast.demand();
		let requests = 0;
		const serving = (async () => {
			for (;;) {
				const request = await wireOf(broadcast).requested();
				if (!request) return;
				requests++;
				request.accept({ timescale: Timescale.MILLI });
			}
		})();

		const open = async () => {
			const client = await Stream.open(pair.client, { version });
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("the publisher never accepted the TRACK stream");
			void publisher.runTrackInfo(new TrackMessage(Path.from("room"), "video"), server);
			return client;
		};
		const first = await open();
		const second = await open();
		await TrackInfo.decode(first.reader, version);
		await TrackInfo.decode(second.reader, version);
		expect(requests).toBe(1);

		first.writer.close();
		expect(await settlesWithin(demand.unused(), 50)).toBe(false);

		second.writer.close();
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		publisher.close();
		broadcast.close();
		await serving;
		origin.close();
	});

	test("a TRACK stream after the cached lookup was let go holds the track again", async () => {
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const broadcast = origin.createBroadcast(Path.from("room"));
		broadcast.announce();
		const demand = broadcast.demand();
		let requests = 0;
		const serving = (async () => {
			for (;;) {
				const request = await wireOf(broadcast).requested();
				if (!request) return;
				requests++;
				request.accept({ timescale: Timescale.MILLI });
			}
		})();

		const open = async () => {
			const client = await Stream.open(pair.client, { version });
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("the publisher never accepted the TRACK stream");
			void publisher.runTrackInfo(new TrackMessage(Path.from("room"), "video"), server);
			return client;
		};
		const first = await open();
		await TrackInfo.decode(first.reader, version);
		first.writer.close();
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		const second = await open();
		await TrackInfo.decode(second.reader, version);
		expect(requests).toBe(2);
		expect(await settlesWithin(demand.unused(), 50)).toBe(false);

		second.writer.close();
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		publisher.close();
		broadcast.close();
		await serving;
		origin.close();
	});

	/** Opens a TRACK or FETCH stream for room/video, served by `publisher`. */
	async function requester(pair: ReturnType<typeof createMockTransportPair>, publisher: Publisher, kind: string) {
		const client = await Stream.open(pair.client, { version });
		const server = await Stream.accept(pair.server, version);
		if (!server) throw new Error(`the publisher never accepted the ${kind} stream`);
		if (kind === "TRACK") void publisher.runTrackInfo(new TrackMessage(Path.from("room"), "video"), server);
		else
			void publisher.runFetch(
				new Fetch({ broadcast: Path.from("room"), track: "video", priority: 0, group: 0 }),
				server,
			);
		return client;
	}

	test.each(["TRACK", "FETCH"])(
		"%s requesters leaving before the answer let the track go once the last one leaves",
		async (kind) => {
			const pair = createMockTransportPair(ALPN_05);
			const origin = new OriginProducer();
			const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
			const broadcast = origin.createBroadcast(Path.from("room"));
			broadcast.announce();
			const demand = broadcast.demand();
			// Answered by hand, after every requester left.
			const requested = wireOf(broadcast).requested();

			const first = await requester(pair, publisher, kind);
			const request = await requested;
			if (!request) throw new Error("the publisher never asked for the track");
			const second = await requester(pair, publisher, kind);

			first.writer.reset(new Error("gone"));
			expect(await settlesWithin(demand.unused(), 50)).toBe(false);

			second.writer.reset(new Error("gone"));
			expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

			// The handler still gets an open track, and the answer lets it go.
			const producer = request.accept({ timescale: Timescale.MILLI });
			expect(producer.closed.peek()).toBeUndefined();
			expect(await settlesWithin(Promise.resolve(producer.closed), 1000)).toBe(true);

			publisher.close();
			broadcast.close();
			origin.close();
		},
	);

	test.each(["TRACK", "FETCH"])("a lost session lets go of a %s still waiting on the answer", async (kind) => {
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const broadcast = origin.createBroadcast(Path.from("room"));
		broadcast.announce();
		const demand = broadcast.demand();
		const requested = wireOf(broadcast).requested();

		await requester(pair, publisher, kind);
		const request = await requested;
		if (!request) throw new Error("the publisher never asked for the track");
		expect(demand.used.peek()).toBe(true);

		pair.server.close();
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		request.reject();
		publisher.close();
		broadcast.close();
		origin.close();
	});

	test("a TRACK stream after every earlier one left an unanswered lookup holds the track again", async () => {
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const broadcast = origin.createBroadcast(Path.from("room"));
		broadcast.announce();
		const demand = broadcast.demand();
		// Answered by hand, after the first TRACK stream has left.
		const requested = wireOf(broadcast).requested();

		const first = await requester(pair, publisher, "TRACK");
		const request = await requested;
		if (!request) throw new Error("the publisher never asked for the track");
		first.writer.reset(new Error("gone"));
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		// The next TRACK stream joins the request still waiting on the application.
		const second = await requester(pair, publisher, "TRACK");
		expect(demand.used.peek()).toBe(true);
		expect(await settlesWithin(wireOf(broadcast).requested(), 50)).toBe(false);
		request.accept({ timescale: Timescale.MILLI });
		await TrackInfo.decode(second.reader, version);
		expect(await settlesWithin(demand.unused(), 50)).toBe(false);

		second.writer.close();
		expect(await settlesWithin(demand.unused(), 1000)).toBe(true);

		publisher.close();
		broadcast.close();
		origin.close();
	});
});
