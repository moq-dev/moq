// Group IDs past 2^53 decode, survive a JS forwarding node, and encode again on every wire
// version, as a third-party moq-transport publisher's or a fine timescale's start ticks may be.
import { expect, test } from "bun:test";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { wireOf } from "./wire.ts";

// The first group ID a `number` cannot hold, and the largest a QUIC varint carries.
const PAST_SAFE = 2n ** 53n + 1n;
const MAX_QUIC = 2n ** 62n - 1n;

const url = new URL("https://localhost:4443/test");
const enc = new TextEncoder();
const dec = new TextDecoder();

/** A negotiated wire: the protocol to offer, an explicit version where no ALPN names one. */
interface Wire {
	name: string;
	protocol: string;
	version?: number;
}

const WIRES: Wire[] = [
	{ name: "lite-01", protocol: "", version: Lite.Version.DRAFT_01 },
	{ name: "lite-02", protocol: "", version: Lite.Version.DRAFT_02 },
	{ name: "lite-03", protocol: Lite.ALPN_03 },
	{ name: "lite-04", protocol: Lite.ALPN_04 },
	{ name: "lite-05", protocol: Lite.ALPN_05 },
	{ name: "lite-06", protocol: Lite.ALPN_06 },
	{ name: "lite-07-wip", protocol: Lite.ALPN_07_WIP },
	{ name: "ietf-18", protocol: Ietf.ALPN.DRAFT_18 },
	{ name: "ietf-20", protocol: Ietf.ALPN.DRAFT_20 },
	{ name: "ietf-22", protocol: Ietf.ALPN.DRAFT_22 },
];

/**
 * Copy `input`'s video track into a broadcast this node produced, keeping every group's
 * ID, as a relay does. A JS origin refuses to serve a broadcast a session delivered.
 */
async function forward(input: BroadcastConsumer): Promise<BroadcastProducer> {
	const subscription = input.track("video").subscribe({ delay: { max: null } });
	const copy = new BroadcastProducer();
	const video = copy.createTrack("video", await subscription.info());
	void (async () => {
		for (let group = await subscription.recvGroup(); group; group = await subscription.recvGroup()) {
			const out = video.createGroup(group.sequence);
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) out.writeFrame(frame);
			out.finish();
		}
	})().catch((err: unknown) => video.abort(err instanceof Error ? err : new Error(String(err))));
	return copy;
}

for (const wire of WIRES) {
	test(`${wire.name}: group IDs past 2^53 cross a JS relay hop`, async () => {
		const upstream = createMockTransportPair(wire.protocol);
		const downstream = createMockTransportPair(wire.protocol);
		const source = new OriginProducer();
		const relay = new OriginProducer();
		const [publisher, ingest, serve, viewer] = await Promise.all([
			connect({ url, transport: upstream.client, publish: source.consume() }),
			accept({ transport: upstream.server, url, version: wire.version }),
			accept({ transport: downstream.server, url, version: wire.version, publish: relay.consume() }),
			connect({ url, transport: downstream.client }),
		]);

		const path = Path.from("big");
		const publication = source.createBroadcast(path);
		publication.announce();
		const track = publication.broadcast.createTrack("video");

		const dynamic = relay.requestsRoute(path);
		const inputs: BroadcastConsumer[] = [];
		const forwarding = (async () => {
			for await (const request of dynamic.requested()) {
				const input = wireOf(ingest).consume(request.path);
				inputs.push(input);
				request.serve(await forward(input));
			}
		})();

		const remote = wireOf(viewer).consume(path);
		const reader = remote.track("video").subscribe({ delay: { max: null } });
		// Publish once the viewer's subscription reached the publisher through the relay, so
		// every group rides it live rather than racing it.
		const demand = track.demand();
		while (demand.wanted.peek() === undefined) await demand.wanted.changed();

		for (const sequence of [PAST_SAFE, MAX_QUIC]) {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: enc.encode(`first ${sequence}`) });
			group.writeFrame({ payload: enc.encode(`second ${sequence}`) });
			group.finish();

			const received = await reader.recvGroup();
			expect(received?.sequence).toBe(sequence);
			expect(dec.decode((await received?.readFrame())?.payload)).toBe(`first ${sequence}`);
			expect(dec.decode((await received?.readFrame())?.payload)).toBe(`second ${sequence}`);
			expect(await received?.readFrame()).toBeUndefined();
			received?.close();
		}

		reader.close();
		remote.close();
		publication.broadcast.close();
		for (const input of inputs) input.close();
		dynamic.close();
		await forwarding;
		for (const session of [publisher, ingest, serve, viewer]) session.abort();
	});
}

test("a group sequence that is not a non-negative bigint is refused where a caller passes one", async () => {
	const track = new BroadcastProducer().createTrack("video");
	// A plain JS caller can still pass a number, which would otherwise mix with bigints later.
	expect(() => track.createGroup(1 as unknown as bigint)).toThrow(TypeError);
	expect(() => track.createGroup(-1n)).toThrow(RangeError);
	expect(track.createGroup(MAX_QUIC).sequence).toBe(MAX_QUIC);
	const consumer = new BroadcastProducer().consume().track("video");
	await expect(consumer.fetchGroup(1 as unknown as bigint)).rejects.toThrow(TypeError);
});
