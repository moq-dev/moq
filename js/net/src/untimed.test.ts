// A track keeps its timedness end to end: no receive path fills in arrival time, and a
// wire that can't declare a timeline delivers frames untimed. Mirrors rs/moq-net/tests/untimed.rs.
import { expect, test } from "bun:test";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

/** A negotiated wire: the protocol to offer, and an explicit version where no ALPN names one. */
interface Wire {
	name: string;
	protocol: string;
	version?: number;
	/** Whether the wire can declare a track's timeline. */
	declares: boolean;
	/** Whether the wire carries a timestamp on every frame, a send time for an untimed one. */
	stamps: boolean;
}

const WIRES: Wire[] = [
	{ name: "lite-01", protocol: "", version: Lite.Version.DRAFT_01, declares: false, stamps: false },
	{ name: "lite-03", protocol: Lite.ALPN_03, declares: false, stamps: false },
	{ name: "lite-04", protocol: Lite.ALPN_04, declares: false, stamps: false },
	{ name: "lite-05", protocol: Lite.ALPN_05, declares: true, stamps: true },
	{ name: "lite-06", protocol: Lite.ALPN_06, declares: true, stamps: true },
	{ name: "lite-07-wip", protocol: Lite.ALPN_07_WIP, declares: true, stamps: true },
	{ name: "ietf-14", protocol: "", version: Ietf.Version.DRAFT_14, declares: false, stamps: false },
	{ name: "ietf-16", protocol: Ietf.ALPN.DRAFT_16, declares: false, stamps: false },
	{ name: "ietf-17", protocol: Ietf.ALPN.DRAFT_17, declares: true, stamps: false },
	{ name: "ietf-20", protocol: Ietf.ALPN.DRAFT_20, declares: true, stamps: false },
];

/** Serve one group of `timestamps` on a track with `timescale`, and read it back over `wire`. */
async function roundTrip(wire: Wire, timescale: Timescale | undefined, timestamps: (Timestamp | undefined)[]) {
	const pair = createMockTransportPair(wire.protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: wire.version, publish: origin.consume() }),
	]);

	const broadcast = origin.createBroadcast(Path.from("test"));
	broadcast.announce();
	// Every request is answered, including the TRACK lookup lite-05+ makes before subscribing.
	const serving = (async () => {
		for (;;) {
			const request = await wireOf(broadcast).requested();
			if (!request) break;
			const group = request.accept({ timescale }).appendGroup();
			for (const [i, timestamp] of timestamps.entries()) {
				group.writeFrame({ payload: new Uint8Array([i]), timestamp });
			}
			group.close();
		}
	})();

	const remote = wireOf(client).consume(Path.from("test"));
	const subscriber = remote.track("video").subscribe();
	const info = await subscriber.info();
	const group = await subscriber.ordered().nextGroup();
	const frames = [];
	for (let frame = await group?.readFrame(); frame; frame = await group?.readFrame()) frames.push(frame);

	broadcast.close();
	await serving;
	remote.close();
	client.abort();
	server.abort();
	return { info, frames };
}

test("a forwarded untimed track stays untimed", async () => {
	// The publisher's hop (lite-04) can't declare a timeline, and the forwarding node must
	// not claim one on its moq-transport hop either.
	const upstream = createMockTransportPair(Lite.ALPN_04);
	const downstream = createMockTransportPair(Ietf.ALPN.DRAFT_20);
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const [publisher, ingest, serve, subscriber] = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url }),
		accept({ transport: downstream.server, url, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);

	const broadcast = source.createBroadcast(Path.from("test"));
	broadcast.announce();
	const serving = (async () => {
		for (;;) {
			const request = await wireOf(broadcast).requested();
			if (!request) break;
			const group = request.accept({ timescale: Timescale.MICRO }).appendGroup();
			group.writeFrame({ payload: new Uint8Array([7]), timestamp: Timestamp.fromMicros(1_234) });
			group.close();
		}
	})();

	// Serve the downstream session from the upstream one.
	const dynamic = relay.dynamic(Path.from("test"));
	const forwarding = (async () => {
		for await (const request of dynamic.requested()) request.accept(wireOf(ingest).consume(request.path));
	})();

	const remote = wireOf(subscriber).consume(Path.from("test"));
	const track = remote.track("video").subscribe();
	expect((await track.info()).timescale).toBeUndefined();
	const frame = await (await track.ordered().nextGroup())?.readFrame();
	expect(frame?.payload).toEqual(new Uint8Array([7]));
	expect(frame?.timestamp).toBeUndefined();

	broadcast.close();
	await serving;
	remote.close();
	dynamic.close();
	await forwarding;
	for (const session of [publisher, ingest, serve, subscriber]) session.abort();
});

for (const wire of WIRES) {
	test(`${wire.name}: an untimed track arrives untimed, or with send times where the wire must stamp`, async () => {
		const { info, frames } = await roundTrip(wire, undefined, [undefined, undefined]);
		expect(frames.map((frame) => frame.payload[0])).toEqual([0, 1]);

		if (wire.stamps) {
			// Lite-05 and lite-06 can't say a track is untimed, so it goes out timed at milliseconds.
			expect(info.timescale).toBe(Timescale.MILLI);
			for (const frame of frames) expect(frame.timestamp).toBeDefined();
		} else {
			expect(info.timescale).toBeUndefined();
			for (const frame of frames) expect(frame.timestamp).toBeUndefined();
		}
	});

	test(`${wire.name}: a timed track keeps its timestamps only where the wire declares units`, async () => {
		const { info, frames } = await roundTrip(wire, Timescale.MICRO, [
			Timestamp.fromMicros(1_000),
			Timestamp.fromMicros(1_234),
		]);
		expect(frames.map((frame) => frame.payload[0])).toEqual([0, 1]);

		if (wire.declares) {
			expect(info.timescale).toBe(Timescale.MICRO);
			expect(frames.map((frame) => frame.timestamp?.asMicros())).toEqual([1_000, 1_234]);
		} else {
			// No units on the wire, and no arrival time made up in their place.
			expect(info.timescale).toBeUndefined();
			for (const frame of frames) expect(frame.timestamp).toBeUndefined();
		}
	});
}
