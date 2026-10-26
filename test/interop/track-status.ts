// Answer Rust-encoded TRACK_STATUS requests with the JS publisher, returning each response.
import assert from "node:assert/strict";
import { Milli } from "@moq/clock";
import { Producer as GroupProducer } from "../../js/net/src/group.ts";
import { NativeSession } from "../../js/net/src/ietf/adapter.ts";
import { Publisher } from "../../js/net/src/ietf/publisher.ts";
import { TrackStatusRequest } from "../../js/net/src/ietf/track.ts";
import type { IetfVersion } from "../../js/net/src/ietf/version.ts";
import { createMockTransportPair } from "../../js/net/src/mock.ts";
import { Producer as OriginProducer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { Stream } from "../../js/net/src/stream.ts";
import { Timescale, Timestamp } from "../../js/net/src/time.ts";

// Stdout is the response-byte channel back to Rust.
console.debug = console.error;
console.info = console.error;
const input: { draft: number; requests: number[][] } = JSON.parse(process.argv[2]);
const version = (0xff000000 + input.draft) as IetfVersion;

const pair = createMockTransportPair();
const origin = new OriginProducer();
const broadcastPublication = origin.createBroadcast(Path.from("room"));
const broadcast = broadcastPublication.broadcast;
broadcastPublication.announce();
const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: Milli(5000), priority: 2 });
const group = new GroupProducer(4);
group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(0) });
group.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.fromMillis(1) });
track.writeGroup(group);

const publisher = new Publisher({
	quic: pair.server,
	session: new NativeSession(pair.server, version, true),
	publish: origin.consume(),
	requiresSolicitation: false,
});

const responses: number[][] = [];
for (const request of input.requests) {
	const written: number[] = [];
	const stream = new Stream({
		// The requester holds its side open until answered: a FIN cancels on drafts 14-18.
		readable: new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(Uint8Array.from(request));
			},
		}),
		writable: new WritableStream<Uint8Array>({
			write(chunk) {
				written.push(...chunk);
			},
		}),
		version,
	});
	assert.equal(await stream.reader.u62(), BigInt(TrackStatusRequest.id));
	const msg = await TrackStatusRequest.decode(stream.reader, version);
	await publisher.runTrackStatusRequest(msg, stream);
	responses.push(written);
}

publisher.close();
origin.close();
process.stdout.write(JSON.stringify(responses));
