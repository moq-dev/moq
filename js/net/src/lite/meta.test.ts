// A metadata subscription crosses each relay hop as a metadata subscription where the hop carries
// the mode (lite-07), so no payload crosses a hop no reader needs it on, and as a payload
// subscription whose payloads the relay drops only where it does not. A publisher's session feeds a
// relay's received copy, which serves a viewer's session, on mock transports and a manual clock.
// Mirrors rs/moq-net/src/lite/meta_tests.rs.

import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { Duration, timeout } from "@moq/clock";
import type { Manual } from "@moq/clock/test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { accept, connect } from "../connection/index.ts";
import { defined } from "../defined.test-util.ts";
import { Producer as GroupProducer } from "../group.ts";
import { WireRoute } from "../hop.ts";
import * as Identity from "../identity.ts";
import { hooks } from "../internal.ts";
import type * as Meta from "../meta.ts";
import { createMockTransportPair, manual } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { InfoInit } from "../track.ts";
import { trackMetaOf, wireOf } from "../wire.ts";
import { Subscribe } from "./subscribe.ts";
import { ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");
const path = Path.from("room");
// A frame big enough that its payload crossing a hop would show.
const BIG = 1000;
const TIMEOUT = Duration.millis(10_000);

afterEach(() => mock.restore());

// Every SUBSCRIBE either hop decodes for the track, with the version it was decoded at, and how many
// frames with their payload either hop's publisher served: each one crossed a hop.
function tap(): { heard: { version: Version; meta: boolean }[]; payloads: () => number } {
	const heard: { version: Version; meta: boolean }[] = [];
	const decode = Subscribe.decode.bind(Subscribe);
	spyOn(Subscribe, "decode").mockImplementation(async (r, version) => {
		const msg = await decode(r, version);
		if (msg.track === "video") heard.push({ version, meta: msg.meta });
		return msg;
	});
	let payloads = 0;
	const read = hooks.readGroupFrame;
	spyOn(hooks, "readGroupFrame").mockImplementation((group, compressed, from, header) => {
		const frame = read(group, compressed, from, header);
		if (!header)
			void frame.then(
				(frame) => {
					if (frame) payloads++;
				},
				() => {},
			);
		return frame;
	});
	return { heard, payloads: () => payloads };
}

// A publisher's session on `up` to a relay, and the relay's session on `down` to a viewer. By `via`,
// the relay serves the viewer its session's broadcast itself, or the logical front a route of that
// broadcast lands in.
async function chain(up: Version, down: Version, via: (typeof VIA)[number], info: InfoInit = {}) {
	const alpn = (version: Version) => (version === Version.DRAFT_07 ? ALPN_07_WIP : ALPN_06);
	const upstream = createMockTransportPair(alpn(up));
	const downstream = createMockTransportPair(alpn(down));
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const table = new OriginProducer();
	const sessions = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url, version: up }),
		accept({ transport: downstream.server, url, version: down, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);
	const [, ingest, , viewer] = sessions;
	// A carried identity, so the route's request resolves to a front.
	const identity = Identity.parse("01900000-0000-7000-8000-000000000001");
	const received = wireOf(table).receive(path, WireRoute.normalize({ identity, cost: 1n }));
	const routing = (async () => {
		for await (const request of received.requested()) request.serve(wireOf(ingest).consume(request.path));
	})();
	const routed = table.consume().request(path);
	const claim = relay.claim(path);
	const serving = (async () => {
		for await (const request of claim.requested()) {
			if (via === "session") {
				request.serve(wireOf(ingest).consume(request.path));
				continue;
			}
			const front = routed.active.peek();
			if (front) request.serve(front.clone());
			else request.reject(new Error("no route"));
		}
	})();
	const broadcast = source.route(path).broadcast;
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null, ...info }));
	const remote = wireOf(viewer).consume(path);
	return {
		remote,
		track,
		// A group at `at` ms, its key, of one big frame and an empty one after it.
		write(at: number) {
			const group = track.createGroup({ payload: new Uint8Array(BIG), timestamp: Timestamp.fromMillis(at) });
			group.writeFrame({ payload: new Uint8Array(0), timestamp: Timestamp.fromMillis(at + 1) });
			group.finish();
		},
		async close() {
			remote.close();
			broadcast.close();
			claim.close();
			routed.close();
			received.close();
			await Promise.all([serving, routing]);
			for (const session of sessions) session.abort();
			for (const origin of [source, relay, table]) origin.close();
		},
	};
}

// The next group's key and each frame's declared size and timestamp in ms, read as a metadata
// reader reads them: headers alone.
async function headers(clock: Manual, reader: Meta.Subscriber): Promise<[bigint, [number, number][]]> {
	const group = defined(await clock.drive(timeout(TIMEOUT, reader.recvGroup())));
	const frames: [number, number][] = [];
	for (;;) {
		const header = await clock.drive(timeout(TIMEOUT, group.nextHeader()));
		if (!header) break;
		frames.push([header.size, header.timestamp.asMillis()]);
	}
	return [group.start, frames];
}

const VIA = ["session", "front"] as const;

test.each(VIA)("metadata crosses lite-07 hops without payload via the relay's %s", async (via) => {
	using clock = manual();
	const { heard, payloads } = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07, via);
	try {
		chained.write(0);
		const reader = chained.remote.meta("video").subscribe();
		expect(await headers(clock, reader)).toEqual([
			0n,
			[
				[BIG, 0],
				[0, 1],
			],
		]);
		chained.write(10);
		expect(await headers(clock, reader)).toEqual([
			10n,
			[
				[BIG, 10],
				[0, 11],
			],
		]);
		// Each hop asked with a metadata subscription, so no payload crossed either.
		expect(heard.length).toBeGreaterThanOrEqual(2);
		expect(heard.every((subscribe) => subscribe.meta)).toBe(true);
		expect(payloads()).toBe(0);
		reader.close();
	} finally {
		await chained.close();
	}
});

test.each(VIA)("metadata pulls payload only across an older hop via the relay's %s", async (via) => {
	using clock = manual();
	const { heard, payloads } = tap();
	const chained = await chain(Version.DRAFT_06, Version.DRAFT_07, via);
	try {
		chained.write(0);
		const reader = chained.remote.meta("video").subscribe();
		expect(await headers(clock, reader)).toEqual([
			0n,
			[
				[BIG, 0],
				[0, 1],
			],
		]);
		// The lite-06 hop cannot carry the mode, so the relay pulls the payload across it alone.
		expect(payloads()).toBe(2);
		const up = heard.filter((subscribe) => subscribe.version === Version.DRAFT_06);
		const down = heard.filter((subscribe) => subscribe.version === Version.DRAFT_07);
		expect(up.length).toBeGreaterThan(0);
		expect(up.every((subscribe) => !subscribe.meta)).toBe(true);
		expect(down.length).toBeGreaterThan(0);
		expect(down.every((subscribe) => subscribe.meta)).toBe(true);
		reader.close();
	} finally {
		await chained.close();
	}
});

test.each(VIA)("readers share the payload subscription across an older hop via the relay's %s", async (via) => {
	using clock = manual();
	const { heard, payloads } = tap();
	const chained = await chain(Version.DRAFT_06, Version.DRAFT_07, via);
	try {
		const payload = chained.remote.track("video").subscribe();
		const meta = chained.remote.meta("video").subscribe();
		chained.write(0);
		const group = defined(await clock.drive(timeout(TIMEOUT, payload.recvGroup())));
		const frame = defined(await clock.drive(timeout(TIMEOUT, group.readFrame())));
		expect(frame.payload.byteLength).toBe(BIG);
		expect(await headers(clock, meta)).toEqual([
			0n,
			[
				[BIG, 0],
				[0, 1],
			],
		]);
		await clock.advance(Duration.millis(1000));
		// The lite-06 hop carries one payload subscription both readers share, so each frame crosses
		// it once, and crosses the lite-07 hop once, to the payload reader.
		expect(heard.filter((subscribe) => subscribe.version === Version.DRAFT_06).length).toBe(1);
		expect(payloads()).toBe(4);
		group.close();
		payload.close();
		meta.close();
	} finally {
		await chained.close();
	}
});

test.each(VIA)("a payload reader and a metadata reader each get their own via the relay's %s", async (via) => {
	using clock = manual();
	const { payloads } = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07, via);
	try {
		chained.write(0);
		const payload = chained.remote.track("video").subscribe();
		const meta = chained.remote.meta("video").subscribe();
		const group = defined(await clock.drive(timeout(TIMEOUT, payload.recvGroup())));
		const frame = defined(await clock.drive(timeout(TIMEOUT, group.readFrame())));
		expect(frame.payload.byteLength).toBe(BIG);
		expect(await headers(clock, meta)).toEqual([
			0n,
			[
				[BIG, 0],
				[0, 1],
			],
		]);

		// Once the payload reader leaves, the metadata reader still gets every header while no
		// payload crosses either hop.
		group.close();
		payload.close();
		await clock.advance(Duration.millis(1000));
		const before = payloads();
		chained.write(10);
		expect(await headers(clock, meta)).toEqual([
			10n,
			[
				[BIG, 10],
				[0, 11],
			],
		]);
		await clock.advance(Duration.millis(1000));
		expect(payloads()).toBe(before);
		meta.close();
	} finally {
		await chained.close();
	}
});

// A broadcast whose handler fills metadata copies keeps each name's two copies apart: a payload
// lookup never gets the metadata copy, nor a metadata lookup the payload one.
test("a payload lookup never gets the metadata copy", async () => {
	const broadcast = new BroadcastProducer();
	const consumer = broadcast.consume();
	hooks.serveMeta(consumer);
	// A handler is open, so each lookup queues a request rather than failing.
	const pulled = wireOf(broadcast).requested();
	const meta = trackMetaOf("video", consumer).subscribe();
	const metaRequest = defined(await pulled);
	expect(metaRequest.meta).toBe(true);
	const metaTrack = metaRequest.accept({ timescale: Timescale.MILLI });
	// The payload lookup asks afresh, and an application track answers that request alone.
	const payload = consumer.track("video").subscribe();
	const payloadTrack = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));

	// Each copy's groups are its own: payload frames for the payload reader, headers for the other.
	const written = hooks.createEmpty(payloadTrack, 0n);
	written.writeFrame({ payload: new Uint8Array(5), timestamp: Timestamp.fromMillis(0) });
	written.finish();
	const header = hooks.createEmpty(metaTrack, 0n);
	expect(() => header.writeFrame({ payload: new Uint8Array(5), timestamp: Timestamp.fromMillis(0) })).toThrow();
	hooks.writeGroupHeader(header, Timestamp.fromMillis(0), 7);
	header.finish();

	const paid = defined(await payload.recvGroup());
	expect(defined(await paid.readFrame()).payload.byteLength).toBe(5);
	const headed = defined(await meta.recvGroup());
	const read = defined(await hooks.readGroupFrame(headed, false, 0, true));
	expect([read.frame.payload.byteLength, read.inflated]).toEqual([0, 7]);
	payload.close();
	meta.close();
	broadcast.close();
});

// Without a handler that fills metadata copies, a metadata lookup reads the payload copy's headers.
test("a metadata lookup reads the payload copy where none is kept apart", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI }));
	const group = hooks.createEmpty(track, 0n);
	group.writeFrame({ payload: new Uint8Array(9), timestamp: Timestamp.fromMillis(0) });
	group.finish();
	const reader = trackMetaOf("video", broadcast.consume()).subscribe();
	const read = defined(await hooks.readGroupFrame(defined(await reader.recvGroup()), false, 0, true));
	expect(read.frame.payload.byteLength).toBe(9);
	reader.close();
	broadcast.close();
});

// A metadata group holds headers alone and a payload group frames alone; a reader gets each
// header's declared size beside an empty payload, and the header costs the cache no payload.
test("a metadata group takes headers alone", () => {
	const meta = new GroupProducer(0n);
	hooks.metaGroup(meta);
	hooks.bindGroupCompression(meta, true);
	expect(() => meta.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.fromMillis(0) })).toThrow();
	expect(() => meta.createFrame({ size: 1, timestamp: Timestamp.fromMillis(0) })).toThrow();
	const reader = meta.consume();
	hooks.writeGroupHeader(meta, Timestamp.fromMillis(0), 1_000_000);
	expect(hooks.groupCacheBytes(meta)).toBeLessThan(1_000);
	const frame = reader.tryReadFrame();
	expect(frame?.payload.byteLength).toBe(0);

	const plain = new GroupProducer(0n);
	expect(() => hooks.writeGroupHeader(plain, Timestamp.fromMillis(0), 1)).toThrow();
	plain.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.fromMillis(0) });
	expect(() => hooks.metaGroup(plain)).toThrow();
});

// A viewer whose own session is older than lite-07 asks for metadata the way a relay does over such
// a hop: one payload subscription, whose payload it drops.
test.each(VIA)("metadata reads headers over an older session via the relay's %s", async (via) => {
	using clock = manual();
	const { heard } = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_06, via);
	try {
		chained.write(0);
		const reader = chained.remote.meta("video").subscribe();
		expect(await headers(clock, reader)).toEqual([
			0n,
			[
				[BIG, 0],
				[0, 1],
			],
		]);
		const down = heard.filter((subscribe) => subscribe.version === Version.DRAFT_06);
		expect(down.length).toBe(1);
		expect(down[0]?.meta).toBe(false);
		reader.close();
	} finally {
		await chained.close();
	}
});

// A metadata reader that wants the payload after all opens a payload subscription at the group it
// reached and reads on from its first missing frame, while the metadata subscription reads on.
test("a payload subscription continues from a metadata position", async () => {
	using clock = manual();
	const { payloads } = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07, "session");
	try {
		const reader = chained.remote.meta("video").subscribe();
		const written = hooks.createEmpty(chained.track, 0n);
		for (const at of [0, 1])
			written.writeFrame({ payload: new Uint8Array(BIG), timestamp: Timestamp.fromMillis(at) });
		const group = defined(await clock.drive(timeout(TIMEOUT, reader.recvGroup())));
		for (let i = 0; i < 2; i++) defined(await clock.drive(timeout(TIMEOUT, group.nextHeader())));
		expect([group.start, group.index]).toEqual([0n, 2]);
		expect(payloads()).toBe(0);

		const payload = chained.remote.track("video").subscribe({ ranges: [{ start: group.start }] });
		written.writeFrame({ payload: new Uint8Array(BIG).fill(7), timestamp: Timestamp.fromMillis(2) });
		written.finish();
		const reading = defined(await clock.drive(timeout(TIMEOUT, payload.recvGroup())));
		expect(reading.start).toBe(group.start);
		const frame = defined(await clock.drive(timeout(TIMEOUT, reading.readFrameIndex({ from: group.index }))));
		expect([frame.index, frame.payload[0]]).toEqual([2, 7]);

		const header = defined(await clock.drive(timeout(TIMEOUT, group.nextHeader())));
		expect([header.size, header.timestamp.asMillis()]).toEqual([BIG, 2]);
		expect(await clock.drive(timeout(TIMEOUT, group.nextHeader()))).toBeUndefined();
		reading.close();
		payload.close();
		reader.close();
	} finally {
		await chained.close();
	}
});

// A history range while only metadata demand stands fills its groups as metadata: every header
// arrives, oldest first, and no payload crosses either hop.
test.each(VIA)("a metadata history fill carries no payload via the relay's %s", async (via) => {
	using clock = manual();
	const { payloads } = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07, via);
	try {
		chained.write(0);
		chained.write(10);
		chained.write(20);
		const reader = chained.remote.meta("video").subscribe({ ranges: [{ order: "ascending" }] });
		for (const at of [0, 10, 20]) {
			expect(await headers(clock, reader)).toEqual([
				BigInt(at),
				[
					[BIG, at],
					[0, at + 1],
				],
			]);
		}
		expect(payloads()).toBe(0);
		reader.close();
	} finally {
		await chained.close();
	}
});

// A compressed track's headers name each frame's plain size, read from a local payload copy and from
// a lite-07 metadata copy alike.
test("metadata names a compressed frame by its plain size", async () => {
	using clock = manual();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07, "session", { compression: "deflate" });
	const local = new BroadcastProducer();
	try {
		const track = defined(local.createTrack("video", { timescale: Timescale.MILLI, compression: "deflate" }));
		const remote = chained.remote.meta("video").subscribe();
		const near = local.consume().meta("video").subscribe();
		for (const producer of [chained.track, track]) {
			const group = hooks.createEmpty(producer, 0n);
			group.writeFrame({ payload: new Uint8Array(BIG), timestamp: Timestamp.fromMillis(0) });
			group.finish();
		}
		expect(await headers(clock, remote)).toEqual([0n, [[BIG, 0]]]);
		expect(await headers(clock, near)).toEqual([0n, [[BIG, 0]]]);
		remote.close();
		near.close();
	} finally {
		local.close();
		await chained.close();
	}
});

// A metadata subscription takes the track's info, an update to its ranges, and the declared end as a
// payload subscription does.
test("a metadata subscription updates and finishes like a payload one", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const reader = broadcast.consume().meta("video").subscribe({ ranges: [] });
	expect((await reader.info()).timescale).toEqual(Timescale.MILLI);
	for (const key of [0n, 1n]) {
		const group = hooks.createEmpty(track, key);
		group.writeFrame({ payload: new Uint8Array(3), timestamp: Timestamp.fromMillis(Number(key)) });
		group.finish();
	}
	track.finishAt({ group: 2n, frame: 0 });
	reader.update({ ranges: [{ order: "ascending" }] });
	for (const key of [0n, 1n]) {
		const group = defined(await reader.recvGroup());
		expect([group.start, defined(await group.nextHeader()).size]).toEqual([key, 3]);
		group.close();
	}
	expect(await reader.finished()).toEqual({ group: 2n, frame: 0 });
	expect(await reader.recvGroup()).toBeUndefined();
	reader.close();
	broadcast.close();
});
