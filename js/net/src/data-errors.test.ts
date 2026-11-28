// A malformed Group Stream at one JS hop, or a violation on the track it forwards, ends only that
// group or track downstream: the session that carries it, and every other broadcast on it, go on.
import { expect, test } from "bun:test";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import { accept, connect } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import { ProtocolViolation, StreamCode, StreamError } from "./error.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { withMax } from "./subscription.test-util.ts";
import { Timestamp } from "./time.ts";
import type * as Track from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");
const enc = new TextEncoder();
const dec = new TextDecoder();

const asError = (err: unknown) => (err instanceof Error ? err : new Error(String(err)));

/**
 * Copy `input`'s video track into a broadcast this node produced, as a forwarding client does: a
 * group that fails upstream fails alone, and the track's own end ends the copy.
 */
async function forward(
	input: BroadcastConsumer,
	failed: (err: unknown) => void,
): Promise<{ copy: BroadcastProducer; video: Track.Producer }> {
	const subscription = input.track("video").subscribe(withMax({ delay: { max: null } }));
	const copy = new BroadcastProducer();
	const video = defined(copy.createTrack("video", await subscription.info()));
	void (async () => {
		for (let group = await subscription.recvGroup(); group; group = await subscription.recvGroup()) {
			const out = video.createGroup(group.start);
			try {
				for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) out.writeFrame(frame);
				out.finish();
			} catch (err: unknown) {
				failed(err);
				out.abort(asError(err));
			}
		}
	})().catch((err: unknown) => video.abort(asError(err)));
	return { copy, video };
}

/** Two broadcasts, `bad` and `good`, from a lite-05 publisher through a JS forwarding hop to a viewer. */
async function chain() {
	const upstream = createMockTransportPair(Lite.ALPN_05);
	const downstream = createMockTransportPair(Lite.ALPN_05);
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const sessions = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url }),
		accept({ transport: downstream.server, url, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);
	const [, ingest, , viewer] = sessions;

	const inputs: BroadcastConsumer[] = [];
	const copies = new Map<string, Track.Producer>();
	// Each group the hop failed to read, in order, as it fails.
	const failures: Promise<unknown>[] = [];
	const waiting: ((err: unknown) => void)[] = [];
	const failed = (err: unknown) => {
		const resolve = waiting.shift();
		if (resolve) resolve(err);
		else failures.push(Promise.resolve(err));
	};
	const nextFailure = () => failures.shift() ?? new Promise<unknown>((resolve) => waiting.push(resolve));
	const claims = ["bad", "good"].map((name) => {
		const claim = relay.claim(Path.from(name));
		void (async () => {
			for await (const request of claim.requested()) {
				const input = wireOf(ingest).consume(request.path);
				inputs.push(input);
				const { copy, video } = await forward(input, failed);
				copies.set(name, video);
				request.serve(copy);
			}
		})();
		return claim;
	});

	const routes = ["bad", "good"].map((name) => source.route(Path.from(name)));
	const tracks = routes.map((route) => defined(route.broadcast.createTrack("video")));
	const remotes = ["bad", "good"].map((name) => wireOf(viewer).consume(Path.from(name)));
	const readers = remotes.map((remote) => remote.track("video").subscribe(withMax({ delay: { max: null } })));
	for (const track of tracks) {
		const demand = track.demand();
		while (!demand.wanted.peek()) await demand.wanted.changed();
	}

	const [badTrack, goodTrack] = tracks;
	const [badReader, goodReader] = readers;
	return {
		badTrack: defined(badTrack),
		goodTrack: defined(goodTrack),
		badReader: defined(badReader),
		goodReader: defined(goodReader),
		copies,
		nextFailure,
		/** The good broadcast still delivers on the downstream session. */
		async goodFlows(key: bigint) {
			const group = defined(goodTrack).createGroup(key);
			group.writeFrame({ payload: enc.encode("good"), timestamp: Timestamp.unscaled(key) });
			group.finish();
			const received = await defined(goodReader).recvGroup();
			expect(dec.decode((await received?.readFrame())?.payload)).toBe("good");
			received?.close();
		},
		dispose() {
			for (const reader of readers) reader.close();
			for (const remote of remotes) remote.close();
			for (const route of routes) route.broadcast.close();
			for (const input of inputs) input.close();
			for (const claim of claims) claim.close();
			for (const session of sessions) session.abort();
		},
	};
}

function malformedTrack(err: unknown) {
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.MalformedTrack);
}

test("a group a JS hop cannot decode ends only that group downstream", async () => {
	const c = await chain();
	try {
		// Lite-05 stamps a scale-less track's frames with milliseconds, which a JS hop decodes only
		// below 2^53: a frame past it is a decode error on that hop's Group Stream.
		for (const key of [2n ** 53n + 1n, 2n ** 53n + 2n]) {
			const bad = c.badTrack.createGroup(key);
			bad.writeFrame({ payload: enc.encode("bad"), timestamp: Timestamp.unscaled(key) });
			bad.finish();
			// The hop reads it as a malformed track, which is all it forwards. A second one arriving
			// proves the first left the track itself up.
			malformedTrack(await c.nextFailure());
		}
		await c.goodFlows(0n);
	} finally {
		c.dispose();
	}
});

test("a violation on a forwarded track ends only that track downstream", async () => {
	const c = await chain();
	try {
		// A group through the hop proves its copy exists.
		const first = c.badTrack.createGroup(0n);
		first.writeFrame({ payload: enc.encode("first"), timestamp: Timestamp.unscaled(0n) });
		first.finish();
		(await c.badReader.recvGroup())?.close();

		// Another session's verdict on the track, as a group past its declared end gives.
		defined(c.copies.get("bad")).abort(new ProtocolViolation("upstream broke the track"));
		malformedTrack(await c.badReader.recvGroup().catch((err: unknown) => err));
		await c.goodFlows(0n);
	} finally {
		c.dispose();
	}
});
