// A track keeps its timedness end to end: no receive path fills in arrival time, a wire that
// can't declare a timeline delivers frames untimed, and lite-05/06, which can't declare an
// untimed one, deliver its send times. Mirrors rs/moq-net/tests/integration/untimed.rs.
import { expect, test } from "bun:test";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import { hooks } from "./internal.ts";
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
	/** Whether a timed track arrives timed. */
	declares: boolean;
	/** Whether an untimed track arrives timed in milliseconds with its send times, as on lite-05/06. */
	sends: boolean;
}

const WIRES: Wire[] = [
	{ name: "lite-01", protocol: "", version: Lite.Version.DRAFT_01, declares: false, sends: false },
	{ name: "lite-03", protocol: Lite.ALPN_03, declares: false, sends: false },
	{ name: "lite-04", protocol: Lite.ALPN_04, declares: false, sends: false },
	{ name: "lite-05", protocol: Lite.ALPN_05, declares: true, sends: true },
	{ name: "lite-06", protocol: Lite.ALPN_06, declares: true, sends: true },
	{ name: "lite-07-wip", protocol: Lite.ALPN_07_WIP, declares: true, sends: false },
	{ name: "ietf-22", protocol: Ietf.ALPN.DRAFT_22, declares: true, sends: false },
];

/** Serve one group of `timestamps` on a track with `timescale`, and read it back over `wire`. */
async function roundTrip(wire: Wire, timescale: Timescale | undefined, timestamps: (Timestamp | undefined)[]) {
	const pair = createMockTransportPair(wire.protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, version: wire.version, publish: origin.consume() }),
	]);

	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	// Every request is answered, including the TRACK lookup lite-05+ makes before subscribing.
	const serving = (async () => {
		for (;;) {
			const request = await wireOf(broadcast).requested();
			if (!request) break;
			const group = request.accept({ timescale }).appendGroup();
			for (const [i, timestamp] of timestamps.entries()) {
				group.writeFrame({ payload: new Uint8Array([i]), timestamp });
			}
			group.finish();
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

/** Copy `input`'s video track into a broadcast this node produced, until `input` closes. */
async function republish(input: BroadcastConsumer): Promise<BroadcastProducer> {
	const subscription = input.track("video").subscribe();
	const copy = new BroadcastProducer();
	const video = copy.createTrack("video", await subscription.info());
	void (async () => {
		try {
			const groups = subscription.ordered();
			for (let group = await groups.nextGroup(); group; group = await groups.nextGroup()) {
				const out = video.appendGroup();
				for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) out.writeFrame(frame);
				out.finish();
			}
			// Closing the broadcast does not end a track inserted into it.
			video.finish();
			video.close();
		} catch (err) {
			// An upstream abort goes downstream as an abort, never as a clean end.
			video.abort(err instanceof Error ? err : new Error(String(err)));
		} finally {
			copy.close();
		}
	})();
	return copy;
}

test("a forwarded untimed track stays untimed", async () => {
	// The publisher's hop (lite-04) can't declare a timeline, and the forwarding node must
	// not claim one on its moq-transport hop either.
	const upstream = createMockTransportPair(Lite.ALPN_04);
	const downstream = createMockTransportPair(Ietf.ALPN.DRAFT_22);
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const [publisher, ingest, serve, subscriber] = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url }),
		accept({ transport: downstream.server, url, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);

	const broadcastPublication = source.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const serving = (async () => {
		for (;;) {
			const request = await wireOf(broadcast).requested();
			if (!request) break;
			const group = request.accept({ timescale: Timescale.MICRO }).appendGroup();
			group.writeFrame({ payload: new Uint8Array([7]), timestamp: Timestamp.fromMicros(1_234) });
			group.finish();
		}
	})();

	// Serve the downstream session from the upstream one. A JS origin refuses to serve a
	// broadcast a session delivered, so the forwarding node republishes a copy it produced.
	const dynamic = relay.requestsRoute(Path.from("test"));
	const inputs: BroadcastConsumer[] = [];
	const forwarding = (async () => {
		for await (const request of dynamic.requested()) {
			const input = wireOf(ingest).consume(request.path);
			inputs.push(input);
			hooks.acceptRequest(request, await republish(input));
		}
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
	for (const input of inputs) input.close();
	dynamic.close();
	await forwarding;
	for (const session of [publisher, ingest, serve, subscriber]) session.abort();
});

for (const wire of WIRES) {
	test(`${wire.name}: an untimed track arrives ${wire.sends ? "with its send times" : "untimed"}`, async () => {
		const { info, frames } = await roundTrip(wire, undefined, [undefined, undefined]);
		expect(frames.map((frame) => frame.payload[0])).toEqual([0, 1]);
		expect(info.timescale).toBe(wire.sends ? Timescale.MILLI : undefined);
		for (const frame of frames) expect(frame.timestamp !== undefined).toBe(wire.sends);
	});

	test(`${wire.name}: a timed track keeps its timestamps only where the wire delivers them`, async () => {
		const { info, frames } = await roundTrip(wire, Timescale.MICRO, [
			Timestamp.fromMicros(1_000),
			Timestamp.fromMicros(1_234),
		]);
		expect(frames.map((frame) => frame.payload[0])).toEqual([0, 1]);

		if (wire.declares) {
			expect(info.timescale).toBe(Timescale.MICRO);
			expect(frames.map((frame) => frame.timestamp?.asMicros())).toEqual([1_000, 1_234]);
		} else {
			// No trusted units on the wire, and no arrival time made up in their place.
			expect(info.timescale).toBeUndefined();
			for (const frame of frames) expect(frame.timestamp).toBeUndefined();
		}
	});
}

for (const wire of WIRES.filter((wire) => wire.name === "lite-06" || wire.name === "lite-07-wip")) {
	test(`${wire.name}: an untimed datagram arrives ${wire.sends ? "with its send time" : "untimed"}`, async () => {
		const pair = createMockTransportPair(wire.protocol);
		const origin = new OriginProducer();
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client }),
			accept({ transport: pair.server, url, publish: origin.consume() }),
		]);

		const broadcastPublication = origin.createBroadcast(Path.from("test"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const producer = broadcast.createTrack("video", {});
		const subscriber = wireOf(client).consume(Path.from("test")).track("video").subscribe();

		// Datagrams aren't cached, so a group read back first proves the subscription is live.
		const group = producer.appendGroup();
		group.writeFrame({ payload: new Uint8Array([0]) });
		group.finish();
		expect(await subscriber.recvGroup()).toBeDefined();

		producer.appendDatagram(undefined, new Uint8Array([7]));
		const datagram = await subscriber.recvDatagram();

		expect(datagram?.payload).toEqual(new Uint8Array([7]));
		expect(datagram?.timestamp !== undefined).toBe(wire.sends);

		broadcast.close();
		subscriber.close();
		client.abort();
		server.abort();
	});
}
