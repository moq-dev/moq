import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Stream } from "../stream.ts";
import { randomOrigin } from "./origin.ts";
import { Publisher } from "./publisher.ts";
import { decodeSubscribeResponse, Subscribe } from "./subscribe.ts";
import { ALPN_05, Version } from "./version.ts";

// Serves a track of `groups` single-frame groups (sequences 0..groups-1), then finishes
// it and returns the SUBSCRIBE_END boundary the publisher put on the wire.
async function subscribeEnd(groups: number): Promise<number> {
	const pair = createMockTransportPair(ALPN_05);
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomOrigin());

	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	publisher.publish(Path.from("test"), broadcast);

	const client = await Stream.open(pair.client);
	const server = await Stream.accept(pair.server);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 });
	void publisher.runSubscribe(msg, server);

	// Finish the track only once it's being served, so the publisher observes a live
	// track ending rather than resolving a subscribe against an already-closed one.
	for (let i = 0; i < groups; i++) {
		track.writeString("hello");
	}
	track.close();

	try {
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_05);
			if ("end" in resp) return resp.end.group;
		}
	} finally {
		broadcast.close();
		client.close();
	}
}

// A Rust subscriber feeds this value straight into `track::Producer::finish_at`, which is
// exclusive, so an inclusive bound here silently truncates the final group across languages.
test("lite draft-05: subscribe end is the exclusive boundary", async () => {
	expect(await subscribeEnd(3)).toBe(3);
});

// 0 is the only encoding for "no groups at all"; an inclusive bound cannot express it
// without colliding with a track whose sole group was sequence 0.
test("lite draft-05: subscribe end is 0 when no groups were produced", async () => {
	expect(await subscribeEnd(0)).toBe(0);
});
