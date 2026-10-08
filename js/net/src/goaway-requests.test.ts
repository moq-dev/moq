import { expect, test } from "bun:test";
import { Once } from "@moq/signals";
import { accept } from "./connection/accept.ts";
import { connect } from "./connection/connect.ts";
import type { Drain } from "./connection/goaway.ts";
import { StreamCode } from "./error.ts";
import type { Session } from "./ietf/adapter.ts";
import { GoAway } from "./ietf/goaway.ts";
import { Subscriber } from "./ietf/subscriber.ts";
import { ALPN, Version as IetfVersion } from "./ietf/version.ts";
import { Goaway } from "./lite/goaway.ts";
import { StreamId } from "./lite/stream.ts";
import { ALPN_06, Version as LiteVersion } from "./lite/version.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
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
	await new Promise((resolve) => setTimeout(resolve, 0));
}

async function waitUntil(pred: () => boolean): Promise<void> {
	const deadline = Date.now() + 2000;
	for (;;) {
		if (pred()) return;
		if (Date.now() > deadline) throw new Error("timed out waiting for condition");
		await settle();
	}
}

/** Bytes of one GOAWAY, framed the way the draft-17 setup stream carries it. */
async function encodeIetfGoaway(): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writable = new WritableStream<Uint8Array>({
		write(chunk) {
			chunks.push(Uint8Array.from(chunk));
		},
	});
	const writer = new Writer(writable, IetfVersion.DRAFT_17);
	await writer.u53(GoAway.id);
	await new GoAway({ newSessionUri: "" }).encode(writer, IetfVersion.DRAFT_17);
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
 * old session and new requests open there instead.
 */
async function handover(kind: "lite" | "ietf"): Promise<void> {
	const protocol = kind === "lite" ? ALPN_06 : ALPN.DRAFT_17;
	const pair = createMockTransportPair(protocol);
	const inject = kind === "ietf" ? captureFirstUni(pair.server) : undefined;
	const goawayBytes = kind === "ietf" ? await encodeIetfGoaway() : undefined;

	const consume = new OriginProducer();
	const publish = new OriginProducer();
	const broadcast = publish.createBroadcast(Path.from("room"));
	broadcast.announce();

	const served: TrackProducer[] = [];
	let armVideo = true;
	const serving = (async () => {
		for (;;) {
			const req = await wireOf(broadcast).requested();
			if (!req) break;
			const track = req.accept({ timescale: Timescale.MILLI });
			if (req.name === "video" && armVideo) track.writeString("one");
			if (req.name === "later") track.writeString("from-draining");
			if (req.name === "audio") track.writeString("from-replacement");
			served.push(track);
		}
	})();

	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client, consume }),
		accept({ transport: pair.server, url, publish: publish.consume() }),
	]);

	const announced = client.announced();
	const request = consume.request(Path.from("room"));
	let replacement: Awaited<ReturnType<typeof connect>> | undefined;
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
			if (producer.closed.peek() === undefined) producer.writeString("two");
		}
		expect(await video.readString()).toBe("two");

		// With no replacement yet, a new subscribe opens on the draining session instead of failing.
		const later = front.track("later").subscribe().ordered();
		expect(await later.readString()).toBe("from-draining");

		// The publisher no longer holds group 0, so its answer proves the FETCH reached the wire.
		const fetched = wireOf(client).consume(Path.from("room")).track("video").fetchGroup(0);
		if (kind === "lite") await expect(fetched).rejects.toMatchObject({ code: StreamCode.NotFound });
		else await expect(fetched).rejects.toThrow(/not supported/);

		// A new announce-interest opens too, and sees the route at the drain cost.
		const interest = client.announced();
		const drained = await interest.next();
		expect(drained?.route.cost).toBe(DRAIN);
		interest.close();

		expect(pair.client.sendStreams.bidi.length).toBeGreaterThan(opened);
		const drainingOpened = pair.client.sendStreams.bidi.length;

		const pair2 = createMockTransportPair(protocol);
		[replacement, replacementServer] = await Promise.all([
			connect({ url, transport: pair2.client, consume }),
			accept({ transport: pair2.server, url, publish: publish.consume() }),
		]);

		await waitUntil(() => {
			const active = request.active.peek();
			return active !== undefined && active !== front;
		});
		const audio = request.active.peek()?.track("audio").subscribe().ordered();
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

test("lite: requests open on the draining session until the replacement wins", async () => {
	await handover("lite");
});

test("ietf: requests open on the draining session until the replacement wins", async () => {
	await handover("ietf");
});

test("announcing and subscribing after GOAWAY still open streams, on the adapter too", async () => {
	for (const version of [IetfVersion.DRAFT_14, IetfVersion.DRAFT_16, IetfVersion.DRAFT_19]) {
		const goaway = new Once<Drain>();
		goaway.set({ uri: "" });
		let opened = false;
		const session: Session = {
			version,
			openBi() {
				opened = true;
				throw new Error("openBi");
			},
			openNativeBi() {
				opened = true;
				throw new Error("openNativeBi");
			},
			acceptBi() {
				return Promise.resolve(undefined);
			},
			nextRequestId() {
				return Promise.resolve(0n);
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
		subscriber.close();
	}
});
