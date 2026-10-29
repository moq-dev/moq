// One upstream subscription carries the aggregate priority of the subscribers it serves, and
// follows it as they come and go. The JS origin never serves a received route onward, so the
// relay case lives with the Rust relay (rs/moq-net/tests/integration/subscribe_priority.rs).

import { expect, spyOn, test } from "bun:test";
import { Manual, yieldNow } from "@moq/clock";
import { accept, connect, type Established } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import { SubscribeUpdate } from "./ietf/subscribe.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Writer } from "./stream.ts";
import { Timescale } from "./time.ts";
import type { Producer as TrackProducer } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");
const path = Path.from("test");

const LOW = -100;
const HIGH = 100;

// Dial a session that consumes what `origin` serves.
async function session(protocol: string, origin: OriginProducer): Promise<[Established, Established]> {
	const pair = createMockTransportPair(protocol);
	lateAcks(pair.client);
	return Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
}

// Acknowledge each write on the client's bidirectional streams a task late, after the peer
// can already have answered it, as a real send stream may.
function lateAcks(transport: MockTransport) {
	const create = transport.createBidirectionalStream.bind(transport);
	transport.createBidirectionalStream = async (options) => {
		const stream = await create(options);
		const inner = stream.writable.getWriter();
		const writable = new WritableStream<Uint8Array>({
			async write(chunk) {
				await inner.write(chunk);
				await new Promise((resolve) => setTimeout(resolve, 0));
			},
			close: () => inner.close(),
			abort: (reason) => inner.abort(reason),
		});
		return { readable: stream.readable, writable };
	};
}

// Resolve once the subscription `track` serves asks for `want`.
async function priority(track: TrackProducer, want: number): Promise<void> {
	while (track.subscription.peek()?.priority !== want) await track.subscription.changed();
}

test.each([Lite.ALPN_07_WIP, ...Object.values(Ietf.ALPN)])(
	"%s follows the aggregate priority upstream",
	async (protocol) => {
		const origin = new OriginProducer();
		const broadcastPublication = origin.createBroadcast(path);
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const [client, server] = await session(protocol, origin);

		const remote = wireOf(client).consume(path);
		const low = remote.track("video").subscribe({ priority: LOW });
		await priority(video, LOW);

		// A second subscriber at a higher priority raises the one upstream request.
		const high = remote.track("video").subscribe({ priority: HIGH });
		await priority(video, HIGH);

		// Leaving lowers it again.
		high.close();
		await priority(video, LOW);
		expect(video.subscription.peek()?.priority).toBe(LOW);

		low.close();
		remote.close();
		broadcast.close();
		client.abort();
		server.abort();
	},
);

// A write may yield to the host after it lands (the shared write budget does under load), so the
// peer's end of a request can arrive between any two writes. With every write yielding, the last
// subscribers leaving while an update is answered must leave only whole messages on the stream:
// a torn one closes the session as malformed.
test.each(Object.values(Ietf.ALPN))("%s ends a subscription mid-update without a torn message", async (protocol) => {
	const clock = new Manual();
	const installed = clock.install();
	const write = Writer.prototype.write;
	const yielding = spyOn(Writer.prototype, "write").mockImplementation(async function (this: Writer, chunk) {
		await write.call(this, chunk);
		await yieldNow();
	});
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const [client, server] = await session(protocol, origin);
	let closed: Error | null | undefined;
	void client.closed.then((err) => {
		closed = err;
	});
	try {
		const remote = wireOf(client).consume(path);
		const low = remote.track("video").subscribe({ priority: LOW });
		await priority(video, LOW);
		const high = remote.track("video").subscribe({ priority: HIGH });
		await priority(video, HIGH);
		high.close();
		await priority(video, LOW);
		// The publisher has read the update and is answering it as the subscription ends.
		low.close();
		while (video.subscription.peek() !== undefined) await video.subscription.changed();
		// Each hop is a task or two away, so this lets the subscriber read all the publisher wrote.
		for (let i = 0; i < 20; i++) await yieldNow();
		expect(closed).toBeUndefined();
		remote.close();
	} finally {
		yielding.mockRestore();
		broadcast.close();
		client.abort();
		server.abort();
		installed.close();
	}
});

// With no subscriber left there is no aggregate to follow, so the subscription ends without an
// update of defaults first, as on lite and in Rust.
test.each(Object.values(Ietf.ALPN))("%s sends no update when the last subscriber leaves", async (protocol) => {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const [client, server] = await session(protocol, origin);
	try {
		const remote = wireOf(client).consume(path);
		const track = remote.track("video").subscribe({ priority: LOW });
		await priority(video, LOW);
		await track.info();
		track.close();
		while (video.subscription.peek() !== undefined) {
			expect(video.subscription.peek()?.priority).toBe(LOW);
			await video.subscription.changed();
		}
		remote.close();
	} finally {
		broadcast.close();
		client.abort();
		server.abort();
	}
});

// An update that fails to encode is a bug, not a publisher that stopped reading, so it ends the
// track with the error rather than leaving it at a stale priority. Nothing reaches the wire.
test.each(Object.values(Ietf.ALPN))("%s ends the track when an update fails to encode", async (protocol) => {
	const origin = new OriginProducer();
	const broadcastPublication = origin.createBroadcast(path);
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const [client, server] = await session(protocol, origin);
	let closed: Error | null | undefined;
	void client.closed.then((err) => {
		closed = err;
	});
	const broken = new Error("broken update");
	const encode = spyOn(SubscribeUpdate.prototype, "encode").mockRejectedValue(broken);
	try {
		const remote = wireOf(client).consume(path);
		const track = remote.track("video").subscribe({ priority: LOW });
		await priority(video, LOW);
		await track.info();
		const high = remote.track("video").subscribe({ priority: HIGH });
		expect(await track.closed).toBe(broken);
		expect(await high.closed).toBe(broken);
		while (video.subscription.peek() !== undefined) {
			expect(video.subscription.peek()?.priority).toBe(LOW);
			await video.subscription.changed();
		}
		expect(closed).toBeUndefined();
		remote.close();
	} finally {
		encode.mockRestore();
		broadcast.close();
		client.abort();
		server.abort();
	}
});
