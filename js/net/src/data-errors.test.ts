// A malformed Group Stream at one JS hop ends only that group's track downstream: the session
// that carries it, and every other broadcast on it, go on.
import { expect, test } from "bun:test";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import { StreamCode, StreamError } from "./error.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { withMax } from "./subscription.test-util.ts";
import { Timestamp } from "./time.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");
const enc = new TextEncoder();
const dec = new TextDecoder();

/** Copy `input`'s video track into a broadcast this node produced, as a forwarding client does. */
async function forward(input: BroadcastConsumer): Promise<BroadcastProducer> {
	const subscription = input.track("video").subscribe(withMax({ delay: { max: null } }));
	const copy = new BroadcastProducer();
	const video = defined(copy.createTrack("video", await subscription.info()));
	void (async () => {
		for (let group = await subscription.recvGroup(); group; group = await subscription.recvGroup()) {
			const out = video.createGroup(group.start);
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) out.writeFrame(frame);
			out.finish();
		}
	})().catch((err: unknown) => video.abort(err instanceof Error ? err : new Error(String(err))));
	return copy;
}

test("a group a JS hop cannot decode ends only its own track downstream", async () => {
	// Lite-05 stamps a scale-less track's frames with milliseconds, which a JS hop decodes only
	// below 2^53: a frame past it is a decode error on that hop's Group Stream.
	const upstream = createMockTransportPair(Lite.ALPN_05);
	const downstream = createMockTransportPair(Lite.ALPN_05);
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const [publisher, ingest, serve, viewer] = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url }),
		accept({ transport: downstream.server, url, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);

	const inputs: BroadcastConsumer[] = [];
	const claims = ["bad", "good"].map((name) => {
		const path = Path.from(name);
		const claim = relay.claim(path);
		void (async () => {
			for await (const request of claim.requested()) {
				const input = wireOf(ingest).consume(request.path);
				inputs.push(input);
				request.serve(await forward(input));
			}
		})();
		return claim;
	});

	const bad = source.route(Path.from("bad"));
	const good = source.route(Path.from("good"));
	const badTrack = defined(bad.broadcast.createTrack("video"));
	const goodTrack = defined(good.broadcast.createTrack("video"));

	const badRemote = wireOf(viewer).consume(Path.from("bad"));
	const goodRemote = wireOf(viewer).consume(Path.from("good"));
	const badReader = badRemote.track("video").subscribe(withMax({ delay: { max: null } }));
	const goodReader = goodRemote.track("video").subscribe(withMax({ delay: { max: null } }));
	for (const track of [badTrack, goodTrack]) {
		const demand = track.demand();
		while (!demand.wanted.peek()) await demand.wanted.changed();
	}

	const malformed = badTrack.createGroup(2n ** 53n + 1n);
	malformed.writeFrame({ payload: enc.encode("bad"), timestamp: Timestamp.unscaled(malformed.start) });
	malformed.finish();
	// The bad track ends with the hop's malformed-track error, not a session close.
	const failed = await badReader.recvGroup().then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(failed).toBeInstanceOf(StreamError);
	expect((failed as StreamError).code).toBe(StreamCode.MalformedTrack);

	// The other broadcast on the same downstream session still flows.
	const group = goodTrack.createGroup(0n);
	group.writeFrame({ payload: enc.encode("good"), timestamp: Timestamp.unscaled(0n) });
	group.finish();
	const received = await goodReader.recvGroup();
	expect(dec.decode((await received?.readFrame())?.payload)).toBe("good");
	received?.close();

	badReader.close();
	goodReader.close();
	badRemote.close();
	goodRemote.close();
	bad.broadcast.close();
	good.broadcast.close();
	for (const input of inputs) input.close();
	for (const claim of claims) claim.close();
	for (const session of [publisher, ingest, serve, viewer]) session.abort();
});
