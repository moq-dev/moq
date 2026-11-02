import { expect, test } from "bun:test";
import { Instant, Manual, Milli, Nano, now, yieldNow } from "@moq/clock";
import { Once } from "@moq/signals";
import { accept } from "./connection/accept.ts";
import { connect } from "./connection/connect.ts";
import type { Drain } from "./connection/goaway.ts";
import { GoAway } from "./ietf/goaway.ts";
import type { Session } from "./ietf/session.ts";
import { Subscriber } from "./ietf/subscriber.ts";
import { ALPN, Version as IetfVersion } from "./ietf/version.ts";
import { Goaway } from "./lite/goaway.ts";
import { StreamId } from "./lite/stream.ts";
import { ALPN_06, Version as LiteVersion } from "./lite/version.ts";
import { createMockTransportPair, type MockTransport, textFrame } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Stream, Writer } from "./stream.ts";
import { Timescale } from "./time.ts";
import type { Producer as TrackProducer } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://relay.example/room");

// Same ceiling Rust stamps as Cost::DRAIN, and the lite cost varint saturates at.
const DRAIN = 2n ** 62n - 1n;

async function settle() {
	await yieldNow();
}

async function waitUntil(pred: () => boolean): Promise<void> {
	const deadline = Instant.add(now(), Nano.fromMilli(Milli(2000)));
	for (;;) {
		if (pred()) return;
		if (now() > deadline) throw new Error("timed out waiting for condition");
		await settle();
	}
}

/** Bytes of one GOAWAY, framed the way the setup stream carries it. */
async function encodeIetfGoaway(): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writable = new WritableStream<Uint8Array>({
		write(chunk) {
			chunks.push(Uint8Array.from(chunk));
		},
	});
	const writer = new Writer(writable, IetfVersion.DRAFT_22);
	await writer.u53(GoAway.id);
	await new GoAway({ newSessionUri: "" }).encode(writer, IetfVersion.DRAFT_22);
	const size = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
	const bytes = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return bytes;
}

/**
 * The server's setup uni is locked inside the connection. Wrap the first one so the test
 * can append a GOAWAY after SETUP without taking the writer's lock.
 */
function captureFirstUni(transport: MockTransport): { inject(bytes: Uint8Array): Promise<void> } {
	const original = transport.createUnidirectionalStream.bind(transport);
	let write: ((bytes: Uint8Array) => Promise<void>) | undefined;
	let captured = false;
	transport.createUnidirectionalStream = async (options) => {
		const writable = await original(options);
		if (captured) return writable;
		captured = true;
		const writer = writable.getWriter();
		write = (bytes) => writer.write(bytes);
		return new WritableStream<Uint8Array>({
			write: (chunk) => writer.write(chunk),
			close: () => writer.close(),
			abort: (reason) => writer.abort(reason),
		});
	};
	return {
		inject: (bytes) => {
			if (!write) throw new Error("setup stream was not opened");
			return write(bytes);
		},
	};
}

async function sendLiteGoaway(server: WebTransport): Promise<void> {
	const stream = await Stream.open(server, { version: LiteVersion.DRAFT_06 });
	await stream.writer.u53(StreamId.Goaway);
	await new Goaway("").encode(stream.writer, LiteVersion.DRAFT_06);
	stream.writer.close();
}

/**
 * After GOAWAY the old session keeps opening subscribe, fetch, and announce-interest streams
 * while it is the only route, at the drain cost. Once a replacement answers, it outranks the
 * old session and new requests open there instead. These wires carry no epoch, so the
 * replacement is another instance: the request already open stays on the draining session.
 */
async function handover(kind: "lite" | "ietf"): Promise<void> {
	const protocol = kind === "lite" ? ALPN_06 : ALPN.DRAFT_22;
	const pair = createMockTransportPair(protocol);
	const inject = kind === "ietf" ? captureFirstUni(pair.server) : undefined;
	const goawayBytes = kind === "ietf" ? await encodeIetfGoaway() : undefined;

	const consume = new OriginProducer();
	const publish = new OriginProducer();
	const broadcastPublication = publish.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();

	const served: TrackProducer[] = [];
	let armVideo = true;
	const serving = (async () => {
		for (;;) {
			const req = await wireOf(broadcast).requested();
			if (!req) break;
			const track = req.accept({ timescale: Timescale.MILLI });
			if (req.name === "video" && armVideo) track.writeFrame(textFrame("one"));
			if (req.name === "later") track.writeFrame(textFrame("from-draining"));
			if (req.name === "audio") track.writeFrame(textFrame("from-replacement"));
			served.push(track);
		}
	})();

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume }),
		accept({ transport: pair.server, url, publish: publish.consume() }),
	]);

	const announced = client.announced();
	// Watching the origin has the session discover routes into it. Request once the route
	// lands, so the request resolves through the table rather than blind.
	const watching = consume.announced();
	expect((await watching.next())?.kind).toBe("start");
	const request = consume.request(Path.from("room"));
	let replacement: Awaited<ReturnType<typeof connect>> | undefined;
	let fresh: ReturnType<typeof consume.request> | undefined;
	let replacementServer: Awaited<ReturnType<typeof accept>> | undefined;

	try {
		const start = await announced.next();
		expect(start?.kind).toBe("start");
		expect(start?.prefix).toBe(Path.from("room"));

		await waitUntil(() => request.active.peek() !== undefined);
		const front = request.active.peek();
		if (!front) throw new Error("no route");

		let gaps = 0;
		const stop = request.active.subscribe((active) => {
			if (active === undefined) gaps++;
		});

		const video = front.track("video").subscribe().ordered();
		expect(await video.readString()).toBe("one");
		armVideo = false;

		const opened = pair.client.sendStreams.bidi.length;

		if (inject && goawayBytes) await inject.inject(goawayBytes);
		else await sendLiteGoaway(pair.server);
		await wireOf(client).goaway;

		const update = await announced.next();
		expect(update?.kind).toBe("update");
		expect(update?.route.cost).toBe(DRAIN);
		// Still the only route, so the subscription already open keeps its front.
		expect(request.active.peek()).toBe(front);

		for (const producer of served) {
			if (producer.closed.peek() === undefined) producer.writeFrame(textFrame("two"));
		}
		expect(await video.readString()).toBe("two");

		// With no replacement yet, a new subscribe opens on the draining session instead of failing.
		const later = front.track("later").subscribe().ordered();
		expect(await later.readString()).toBe("from-draining");

		// A fresh consumer caches nothing, so the publisher's answer proves the FETCH reached the wire.
		const fetched = wireOf(client).consume(Path.from("room")).track("video").fetchGroup(0);
		if (kind === "lite") expect(await (await fetched).readString()).toBe("one");
		else await expect(fetched).rejects.toThrow(/not supported/);

		// A new announce-interest opens too, and sees the route at the drain cost.
		const interest = client.announced();
		const drained = await interest.next();
		expect(drained?.route.cost).toBe(DRAIN);
		interest.close();

		expect(pair.client.sendStreams.bidi.length).toBeGreaterThan(opened);
		const drainingOpened = pair.client.sendStreams.bidi.length;

		const routes = consume.consume().announced();
		expect((await routes.next())?.kind).toBe("start");

		const pair2 = createMockTransportPair(protocol);
		[replacement, replacementServer] = await Promise.all([
			connect({ url, transport: pair2.client, consume }),
			accept({ transport: pair2.server, url, publish: publish.consume() }),
		]);

		expect(await routes.next()).toMatchObject({ prefix: Path.from("room"), kind: "restart" });
		routes.close();
		expect(request.active.peek()).toBe(front);

		fresh = consume.request(Path.from("room"));
		await waitUntil(() => fresh?.active.peek() !== undefined);
		expect(fresh.active.peek()).not.toBe(front);
		const audio = fresh.active.peek()?.track("audio").subscribe().ordered();
		if (!audio) throw new Error("replacement did not answer");
		expect(await audio.readString()).toBe("from-replacement");
		// Once the replacement wins, nothing new opens on the draining session.
		expect(pair.client.sendStreams.bidi.length).toBe(drainingOpened);
		expect(pair2.client.sendStreams.bidi.length).toBeGreaterThan(0);
		expect(gaps).toBe(0);
		stop();
		audio.close();
		later.close();
		video.close();
	} finally {
		announced.close();
		watching.close();
		request.close();
		fresh?.close();
		replacement?.abort();
		replacementServer?.abort();
		client.abort();
		server.abort();
		broadcast.close();
		await serving;
		publish.close();
		consume.close();
	}
}

test("lite: requests open on the draining session until the replacement wins", async () => {
	await handover("lite");
});

test("ietf: requests open on the draining session until the replacement wins", async () => {
	await handover("ietf");
});

test("announcing and subscribing after GOAWAY still open streams", async () => {
	for (const version of [IetfVersion.DRAFT_18, IetfVersion.DRAFT_22]) {
		const goaway = new Once<Drain>();
		goaway.set({ uri: "" });
		let opened = false;
		const session: Session = {
			version,
			client: true,
			openBi() {
				opened = true;
				throw new Error("openBi");
			},
			acceptBi() {
				return Promise.resolve(undefined);
			},
			nextRequestId() {
				return Promise.resolve(0n);
			},
			checkRequestId() {
				throw new Error("unexpected incoming request");
			},
			close() {},
		};

		const subscriber = new Subscriber({ session, goaway });
		const track = subscriber.consume(Path.from("room")).track("video").subscribe();
		await track.closed;
		expect(opened).toBe(true);

		opened = false;
		const announced = subscriber.announced();
		await waitUntil(() => opened);
		announced.close();
	}
});

/**
 * A route change alone is never fatal, but a dead route is: once the replacement wins, the
 * subscription already open on the draining session keeps going while the old publisher
 * serves it, and that session closing ends the subscription with an error, never cleanly.
 * The old session closes either before the `restart` reaches the viewer or after.
 */
async function routeDeath(kind: "lite" | "ietf", after: boolean): Promise<void> {
	const clock = new Manual();
	const installed = clock.install();
	using _clock = { [Symbol.dispose]: () => installed.close() };
	const protocol = kind === "lite" ? ALPN_06 : ALPN.DRAFT_22;
	const pair = createMockTransportPair(protocol);
	const inject = kind === "ietf" ? captureFirstUni(pair.server) : undefined;
	const goawayBytes = kind === "ietf" ? await encodeIetfGoaway() : undefined;

	const consume = new OriginProducer();
	const publish = new OriginProducer();
	const broadcastPublication = publish.createBroadcast(Path.from("room"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();

	const served: TrackProducer[] = [];
	const serving = (async () => {
		for (;;) {
			const req = await wireOf(broadcast).requested();
			if (!req) break;
			const track = req.accept({ timescale: Timescale.MILLI });
			track.writeFrame(textFrame("one"));
			served.push(track);
		}
	})();

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume }),
		accept({ transport: pair.server, url, publish: publish.consume() }),
	]);

	const watching = consume.announced();
	const request = consume.request(Path.from("room"));
	let routes: ReturnType<typeof consume.announced> | undefined;
	let replacement: Awaited<ReturnType<typeof connect>> | undefined;
	let replacementServer: Awaited<ReturnType<typeof accept>> | undefined;

	try {
		expect((await watching.next())?.kind).toBe("start");
		await waitUntil(() => request.active.peek() !== undefined);
		const front = request.active.peek();
		if (!front) throw new Error("no route");
		const video = front.track("video").subscribe().ordered();
		expect(await video.readString()).toBe("one");

		if (inject && goawayBytes) await inject.inject(goawayBytes);
		else await sendLiteGoaway(pair.server);
		await wireOf(client).goaway;

		routes = consume.announced();
		expect((await routes.next())?.kind).toBe("start");
		const pair2 = createMockTransportPair(protocol);
		[replacement, replacementServer] = await Promise.all([
			connect({ url, transport: pair2.client, consume }),
			accept({ transport: pair2.server, url, publish: publish.consume() }),
		]);
		if (after) {
			// Skip the drain cost reaching the route, if it lands after the start.
			let event = await routes.next();
			while (event?.kind === "update") event = await routes.next();
			expect(event).toMatchObject({ prefix: Path.from("room"), kind: "restart" });
		}

		// The old publisher still serves the old instance, so its subscription carries on.
		for (const producer of served) {
			if (producer.closed.peek() === undefined) producer.writeFrame(textFrame("two"));
		}
		expect(await video.readString()).toBe("two");

		// A draining close of a session still serving a live track gives up after its drain.
		const closing = server.close().catch(() => {});
		await clock.advance(Nano.fromMilli(Milli(1000)));
		await closing;
		await expect(
			(async () => {
				for (;;) {
					if ((await video.readString()) === undefined) return "ended cleanly";
				}
			})(),
		).rejects.toThrow();
		video.close();
	} finally {
		watching.close();
		routes?.close();
		request.close();
		replacement?.abort();
		replacementServer?.abort();
		client.abort();
		server.abort();
		broadcast.close();
		await serving;
		publish.close();
		consume.close();
	}
}

for (const kind of ["lite", "ietf"] as const) {
	test(`${kind}: the draining session closing before the restart errors its subscription`, async () => {
		await routeDeath(kind, false);
	});

	test(`${kind}: the draining session closing after the restart errors its subscription`, async () => {
		await routeDeath(kind, true);
	});
}
