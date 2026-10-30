import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { Milli, yieldNow } from "@moq/clock";
import { type Getter, Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Producer as BroadcastProducer, type Tracks } from "../broadcast.ts";
import { accept, connect } from "../connection/index.ts";
import * as Epoch from "../epoch.ts";
import { fromTransport, NotFound, SessionCode, StreamCode, StreamError } from "../error.ts";
import { randomHop } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type { Limits } from "../util/limits.ts";
import { wireOf } from "../wire.ts";
import { Publisher } from "./publisher.ts";
import { StreamId } from "./stream.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo } from "./track.ts";
import { decodeTracksEventMaybe, encodeTracksEvent, type TracksEvent, Tracks as TracksMessage } from "./tracks.ts";
import { ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");
const version = Version.DRAFT_07;

afterEach(() => mock.restore());

/** Lets queued microtasks, signal notifications, and mock stream deliveries run. */
function settle(): Promise<void> {
	return yieldNow();
}

async function bytes(f: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const chunks: number[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(...chunk) }),
		version,
	);
	await f(writer);
	writer.close();
	await writer.closed;
	return Uint8Array.from(chunks);
}

function format(event: Awaited<ReturnType<Tracks["next"]>>): string {
	if (!event) return "end";
	if (event.kind === "synced") return "synced";
	return `${event.kind === "added" ? "+" : "-"}${event.name}`;
}

/** The next event of a listing, formatted like the model tests, or the error that ended it. */
async function next(tracks: Tracks): Promise<string> {
	try {
		return format(await tracks.next());
	} catch (err) {
		return `error: ${err instanceof StreamError ? err.code : String(err)}`;
	}
}

test("SYNCED is an empty type four, and an unknown event is refused", async () => {
	expect(await bytes((w) => encodeTracksEvent(w, { kind: "synced" }, version))).toEqual(new Uint8Array([4, 0]));
	await expect(
		decodeTracksEventMaybe(new Reader(undefined, new Uint8Array([3, 0]), version), version),
	).rejects.toThrow("unknown tracks event");
	await expect(
		decodeTracksEventMaybe(new Reader(undefined, new Uint8Array([4, 1, 0]), version), version),
	).rejects.toThrow("too few bytes");
});

test("events and the request round-trip", async () => {
	const events: TracksEvent[] = [
		{
			kind: "added",
			name: "video",
			info: new TrackInfo({ priority: 3, refresh: 4, maxAge: 2000, timescale: 1_000_000 }),
		},
		{ kind: "removed", name: "video" },
		{ kind: "finished", name: "audio" },
		{ kind: "synced" },
	];
	const encoded = await bytes(async (w) => {
		for (const event of events) await encodeTracksEvent(w, event, version);
	});
	const reader = new Reader(undefined, encoded, version);
	const added = await decodeTracksEventMaybe(reader, version);
	expect(added).toMatchObject({ kind: "added", name: "video", info: { priority: 3, refresh: 4, maxAge: 2000 } });
	expect(added?.kind === "added" && added.info.timescale).toBe(Timescale(1_000_000));
	expect(await decodeTracksEventMaybe(reader, version)).toEqual({ kind: "removed", name: "video" });
	expect(await decodeTracksEventMaybe(reader, version)).toEqual({ kind: "finished", name: "audio" });
	expect(await decodeTracksEventMaybe(reader, version)).toEqual({ kind: "synced" });
	expect(await decodeTracksEventMaybe(reader, version)).toBeUndefined();

	const epoch = Epoch.parse("01900000-0000-7000-8000-000000000001");
	const request = await bytes((w) => new TracksMessage(Path.from("room/alice"), epoch).encode(w, version));
	const got = await TracksMessage.decode(new Reader(undefined, request, version), version);
	expect(got.broadcast).toBe(Path.from("room/alice"));
	expect(got.epoch).toBe(epoch);
});

test("older versions have no tracks stream", async () => {
	await expect(bytes((w) => new TracksMessage(Path.from("room")).encode(w, Version.DRAFT_06))).rejects.toThrow(
		"not supported",
	);
	await expect(bytes((w) => encodeTracksEvent(w, { kind: "synced" }, Version.DRAFT_06))).rejects.toThrow(
		"not supported",
	);
});

/** A lite-07 subscriber whose publisher the test plays by hand. */
function handPlayed({ limits, grant }: { limits?: Limits; grant?: Getter<Grant | undefined> } = {}) {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const closing = spyOn(pair.client, "close");
	const subscriber = new Subscriber(
		pair.client,
		version,
		randomHop(),
		undefined,
		undefined,
		undefined,
		grant,
		limits,
	);
	// The TRACKS request the subscriber opens next.
	const listed = async () => {
		const stream = await Stream.accept(pair.server, version);
		if (!stream) throw new Error("the subscriber never listed");
		expect(await stream.reader.u53()).toBe(StreamId.Tracks);
		const msg = await TracksMessage.decode(stream.reader, version);
		return {
			msg,
			stream,
			send: (event: TracksEvent) => encodeTracksEvent(stream.writer, event, version),
		};
	};
	return { pair, closing, subscriber, listed };
}

const info = new TrackInfo({ priority: 2, timescale: 1000 });

test("a session syncs at once and forwards the peer's tracks as live adds", async () => {
	const { subscriber, listed } = handPlayed();
	const remote = subscriber.consume(Path.from("room"));
	const tracks = remote.tracks();
	expect(await next(tracks)).toBe("synced");

	const peer = await listed();
	expect(peer.msg.broadcast).toBe(Path.from("room"));
	await peer.send({ kind: "added", name: "a", info });
	await peer.send({ kind: "synced" });
	const added = await tracks.next();
	expect(added).toMatchObject({ kind: "added", name: "a", info: { priority: 2, timescale: Timescale.MILLI } });

	await peer.send({ kind: "removed", name: "a" });
	expect(await next(tracks)).toBe("-a");

	// The peer's broadcast ended: so does the listing, whether or not its retraction arrived.
	peer.stream.writer.close();
	expect(await next(tracks)).toBe("end");
	expect(remote.closed.peek()).toBeUndefined();
	// The requester FINs its half too, so the stream does not linger half-open.
	expect(await peer.stream.reader.done()).toBe(true);
});

test("every listing of a broadcast shares one TRACKS stream, cancelled once unused", async () => {
	const { subscriber, listed, pair } = handPlayed();
	const remote = subscriber.consume(Path.from("room"));
	const first = remote.tracks();
	const peer = await listed();
	await peer.send({ kind: "added", name: "a", info });
	expect(await next(first)).toBe("synced");
	expect(await next(first)).toBe("+a");

	// A second reader, through a shared handle too, joins the same stream with what it holds.
	const second = subscriber.consume(Path.from("room")).tracks();
	expect(await next(second)).toBe("+a");
	expect(await next(second)).toBe("synced");
	let opened = false;
	const another = Stream.accept(pair.server, version).then((stream) => {
		opened = true;
		return stream;
	});
	await settle();
	expect(opened).toBe(false);

	let cancelled = false;
	void peer.stream.reader.done().then(
		() => (cancelled = true),
		() => (cancelled = true),
	);
	first.close();
	await settle();
	expect(cancelled).toBe(false);
	// The last reader leaving ends the requester's side.
	second.close();
	await settle();
	expect(cancelled).toBe(true);

	// A later reader asks again.
	const third = remote.tracks();
	await another;
	expect(opened).toBe(true);
	third.close();
});

test("a peer's malformed listing closes the session", async () => {
	for (const events of [
		[{ kind: "added", name: "a", info } as const, { kind: "added", name: "a", info } as const],
		[{ kind: "removed", name: "a" } as const],
		[{ kind: "synced" } as const, { kind: "synced" } as const],
	]) {
		const { subscriber, listed, closing } = handPlayed();
		const tracks = subscriber.consume(Path.from("room")).tracks();
		const peer = await listed();
		for (const event of events) await peer.send(event);
		let event: string;
		do event = await next(tracks);
		while (event === "synced" || event.startsWith("+"));
		expect(event).toMatch(/^error: /);
		expect(closing).toHaveBeenCalledWith(expect.objectContaining({ closeCode: SessionCode.ProtocolViolation }));
	}
});

test("a listing past the announces cap is reset, and the session stays", async () => {
	const cap = 3;
	const { subscriber, listed, closing } = handPlayed({ limits: { announces: cap } });
	const tracks = subscriber.consume(Path.from("room")).tracks();
	expect(await next(tracks)).toBe("synced");
	const peer = await listed();
	// Read in step: the reset withdraws whatever adds are still unread.
	for (let i = 0; i < cap; i++) {
		await peer.send({ kind: "added", name: `t${i}`, info });
		expect(await next(tracks)).toBe(`+t${i}`);
	}
	await peer.send({ kind: "added", name: `t${cap}`, info });
	expect(await next(tracks)).toMatch(/^error: /);
	await settle();
	expect(closing).not.toHaveBeenCalled();
	// The reset released every slot: another listing lists the whole cap again.
	const again = subscriber.consume(Path.from("room")).tracks();
	const retry = await listed();
	for (let i = 0; i < cap; i++) await retry.send({ kind: "added", name: `t${i}`, info });
	expect(await next(again)).toBe("synced");
	for (let i = 0; i < cap; i++) expect(await next(again)).toBe(`+t${i}`);
	again.close();
});

test("a flood of a peer's listing churn yields to the host's tasks", async () => {
	const { subscriber, listed } = handPlayed();
	const tracks = subscriber.consume(Path.from("room")).tracks();
	const peer = await listed();
	// Churn of one name never reaches the announces cap, so only the budget bounds it.
	const flood = await bytes(async (w) => {
		await encodeTracksEvent(w, { kind: "added", name: "first", info }, version);
		for (let i = 0; i < 20_000; i++) {
			await encodeTracksEvent(w, { kind: "added", name: "churn", info }, version);
			await encodeTracksEvent(w, { kind: "removed", name: "churn" }, version);
		}
		await encodeTracksEvent(w, { kind: "added", name: "last", info }, version);
	});
	await peer.stream.writer.write(flood);
	expect(await next(tracks)).toBe("synced");
	expect(await next(tracks)).toBe("+first");
	// The receiver is mid-flood: a task queued now runs before it reaches the end.
	let last = false;
	let ranFirst: boolean | undefined;
	void yieldNow().then(() => {
		ranFirst = !last;
	});
	let event: string;
	do event = await next(tracks);
	while (event !== "+last");
	last = true;
	await settle();
	expect(ranFirst).toBe(true);
	tracks.close();
});

test("a version without the Tracks Stream refuses the listing", async () => {
	const pair = createMockTransportPair();
	const subscriber = new Subscriber(pair.client, Version.DRAFT_06, randomHop());
	const tracks = subscriber.consume(Path.from("room")).tracks();
	expect(await next(tracks)).toBe("error: Error: track listing requires moq-lite-07 or newer");
});

function scoped(prefix: string): Grant {
	return { publish: new Path.Patterns(), subscribe: new Path.Patterns([Path.Pattern.subtree(prefix)]) };
}

test("a listing our grant does not cover is refused, and never reaches the wire", async () => {
	const { subscriber, pair } = handPlayed({ grant: new Signal<Grant | undefined>(scoped("other")) });
	const tracks = subscriber.consume(Path.from("room")).tracks();
	expect(await next(tracks)).toBe(`error: ${StreamCode.Unauthorized}`);
	expect(pair.client.sendStreams.bidi).toHaveLength(0);
});

test("a grant that stops covering a listing resets it with UNAUTHORIZED, and the session stays", async () => {
	const grant = new Signal<Grant | undefined>(scoped("room"));
	const { subscriber, listed, closing } = handPlayed({ grant });
	const tracks = subscriber.consume(Path.from("room")).tracks();
	const peer = await listed();
	await peer.send({ kind: "added", name: "a", info });
	expect(await next(tracks)).toBe("synced");
	expect(await next(tracks)).toBe("+a");

	grant.set(scoped("other"));
	expect(await next(tracks)).toBe(`error: ${StreamCode.Unauthorized}`);
	const reset = await peer.stream.reader.done().then(
		() => undefined,
		(err: unknown) => fromTransport(err),
	);
	expect((reset as StreamError).code).toBe(StreamCode.Unauthorized);
	expect(closing).not.toHaveBeenCalled();
});

/** A lite-07 publisher serving `origin`, answering TRACKS streams the test writes by hand. */
function publisher(origin: OriginProducer) {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const closing = spyOn(pair.server, "close");
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const list = async (broadcast: string) => {
		const requester = await Stream.open(pair.client, { version });
		void (async () => {
			const stream = await Stream.accept(pair.server, version);
			if (!stream) throw new Error("no stream");
			expect(await stream.reader.u53()).toBe(StreamId.Tracks);
			await publisher.runTracks(await TracksMessage.decode(stream.reader, version), stream);
		})();
		await requester.writer.u53(StreamId.Tracks);
		await new TracksMessage(Path.from(broadcast)).encode(requester.writer, version);
		return {
			requester,
			next: async () => {
				try {
					const event = await decodeTracksEventMaybe(requester.reader, version);
					return event?.kind === "added"
						? `+${event.name}`
						: event?.kind === "removed"
							? `-${event.name}`
							: (event?.kind ?? "end");
				} catch (err) {
					const reset = fromTransport(err);
					return `reset: ${reset instanceof StreamError ? reset.code : reset.message}`;
				}
			},
		};
	};
	return { list, closing };
}

test("data after a TRACKS request closes the session", async () => {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("a");
	const { list, closing } = publisher(origin);
	const listing = await list("room");
	expect(await listing.next()).toBe("+a");
	expect(await listing.next()).toBe("synced");
	await listing.requester.writer.u53(0);
	expect(await listing.next()).toMatch(/^reset: /);
	expect(closing).toHaveBeenCalledWith(expect.objectContaining({ closeCode: SessionCode.ProtocolViolation }));
	broadcast.close();
});

test("a publisher lists its broadcast's own tracks, never one served on request", async () => {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("a", { priority: 5 });
	// Served on request: answered when asked, never listed.
	void wireOf(broadcast)
		.requested()
		.then((request) => request?.accept({}));
	const { list } = publisher(origin);
	const listing = await list("room");
	expect(await listing.next()).toBe("+a");
	expect(await listing.next()).toBe("synced");

	broadcast.consume().track("x").subscribe();
	broadcast.createTrack("b");
	expect(await listing.next()).toBe("+b");

	broadcast.close();
	expect(await listing.next()).toBe("end");
});

test("a newer listing of a broadcast on one session replaces the older", async () => {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("a");
	const { list } = publisher(origin);
	const older = await list("room");
	expect(await older.next()).toBe("+a");
	expect(await older.next()).toBe("synced");
	const newer = await list("room");
	expect(await newer.next()).toBe("+a");
	expect(await newer.next()).toBe("synced");
	expect(await older.next()).toBe(`reset: ${StreamCode.Cancel}`);
	broadcast.createTrack("b");
	expect(await newer.next()).toBe("+b");
	broadcast.close();
});

test("a path served through a covering prefix route is NOT_FOUND", async () => {
	const origin = new OriginProducer();
	const dynamic = origin.requestsRoute(Path.from("room"));
	void (async () => {
		for await (const request of dynamic.requested()) {
			const served = new BroadcastProducer();
			served.createTrack("video");
			hooks.acceptRequest(request, served);
		}
	})();
	const { list } = publisher(origin);
	expect(await (await list("room/alice")).next()).toBe(`reset: ${StreamCode.NotFound}`);
	expect(await (await list("absent")).next()).toBe(`reset: ${StreamCode.NotFound}`);
	dynamic.close();
});

test("a local read through a covering prefix route is NOT_FOUND, and through its exact route lists", async () => {
	const origin = new OriginProducer();
	for (const prefix of ["room", "room/bob"]) {
		const dynamic = origin.requestsRoute(Path.from(prefix));
		void (async () => {
			for await (const request of dynamic.requested()) {
				const served = new BroadcastProducer();
				served.createTrack("video");
				hooks.acceptRequest(request, served);
			}
		})();
	}
	const consumer = origin.consume();
	const alice = consumer.request(Path.from("room/alice"));
	const bob = consumer.request(Path.from("room/bob"));
	await settle();

	const covered = alice.active.peek();
	if (!covered) throw new Error("expected the covering route to serve");
	expect(() => covered.tracks()).toThrow(NotFound);
	// Every handle cloned from it refuses too.
	expect(() => covered.clone().tracks()).toThrow(NotFound);

	const exact = bob.active.peek();
	if (!exact) throw new Error("expected the exact route to serve");
	const tracks = exact.tracks();
	expect(await next(tracks)).toBe("+video");
	expect(await next(tracks)).toBe("synced");
	tracks.close();
	origin.close();
});

test("a listing over a session carries each track's info, end to end", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("a", {
		timescale: Timescale.MILLI,
		maxAge: Milli(3000),
		priority: 9,
		refresh: 4,
		compress: true,
	});

	const tracks = wireOf(client).consume(Path.from("room")).tracks();
	expect(await next(tracks)).toBe("synced");
	expect(await tracks.next()).toEqual({
		kind: "added",
		name: "a",
		info: { timescale: Timescale.MILLI, maxAge: Milli(3000), priority: 9, refresh: 4, compress: true },
	});
	broadcast.createTrack("b");
	expect(await next(tracks)).toBe("+b");
	broadcast.removeTrack("a");
	expect(await next(tracks)).toBe("-a");
	broadcast.close();
	expect(await next(tracks)).toBe("end");
	client.abort();
	server.abort();
});

test("an epoch-pinned front lists its serving route's tracks", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const serverOrigin = new OriginProducer();
	const clientOrigin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume: clientOrigin }),
		accept({ transport: pair.server, url, publish: serverOrigin.consume() }),
	]);
	const broadcastPublication = serverOrigin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcast.createTrack("a");
	broadcastPublication.announce({ epoch: Epoch.parse("01900000-0000-7000-8000-000000000001") });

	const reader = clientOrigin.consume();
	const announced = reader.announced();
	expect(await announced.next()).toMatchObject({ prefix: Path.from("room"), kind: "start" });
	const request = reader.request(Path.from("room"));
	await settle();
	const front = request.active.peek();
	if (!front) throw new Error("expected the origin to route the broadcast");
	const tracks = front.tracks();
	// The front syncs with its source's listing, which syncs at once with what the session holds.
	expect(await next(tracks)).toBe("synced");
	expect(await next(tracks)).toBe("+a");
	broadcast.createTrack("b");
	expect(await next(tracks)).toBe("+b");
	tracks.close();
	request.close();
	announced.close();
	broadcast.close();
	client.abort();
	server.abort();
});

test("an epoch-pinned front served through a covering prefix route is NOT_FOUND", async () => {
	const origin = new OriginProducer();
	const dynamic = origin.requestsRoute(Path.from("room"), {
		epoch: Epoch.parse("01900000-0000-7000-8000-000000000001"),
	});
	void (async () => {
		for await (const request of dynamic.requested()) {
			const served = new BroadcastProducer();
			served.createTrack("video");
			hooks.acceptRequest(request, served);
		}
	})();
	const request = origin.consume().request(Path.from("room/alice"));
	await settle();
	const front = request.active.peek();
	if (!front) throw new Error("expected the covering route to serve");
	expect(await next(front.tracks())).toBe(`error: ${StreamCode.NotFound}`);
	// A later reader asks again, and is refused again.
	expect(await next(front.tracks())).toBe(`error: ${StreamCode.NotFound}`);
	request.close();
	origin.close();
});

test("lite-06 cannot list over a session", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcastPublication = origin.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const tracks = wireOf(client).consume(Path.from("room")).tracks();
	expect(await next(tracks)).toMatch(/requires moq-lite-07/);
	broadcast.close();
	client.abort();
	server.abort();
});
