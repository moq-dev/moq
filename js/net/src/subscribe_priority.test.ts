// One upstream subscription carries the aggregate priority of the subscribers it serves, and
// follows it as they come and go. The JS origin never serves a received route onward, so the
// relay case lives with the Rust relay (rs/moq-net/tests/integration/subscribe_priority.rs).

import { expect, test } from "bun:test";
import { accept, connect, type Established } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, type MockTransport } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale } from "./time.ts";
import type { Producer as TrackProducer } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");
const path = Path.from("test");

const LOW = 1;
const HIGH = 200;

// Dial a session that consumes what `origin` serves. Draft-14's ALPN is the generic one, so
// the server is told the version instead.
async function session(protocol: string, origin: OriginProducer): Promise<[Established, Established]> {
	const draft14 = protocol === Ietf.ALPN.DRAFT_14;
	const pair = createMockTransportPair(draft14 ? "" : protocol);
	lateAcks(pair.client);
	return Promise.all([
		connect({ url, transport: pair.client }),
		accept({
			transport: pair.server,
			url,
			publish: origin.consume(),
			version: draft14 ? Ietf.Version.DRAFT_14 : undefined,
		}),
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
		const broadcast = origin.createBroadcast(path);
		broadcast.announce();
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
