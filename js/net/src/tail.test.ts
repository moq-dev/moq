import { expect, test } from "bun:test";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { TAIL_GRACE_MS } from "./tail.ts";
import { Milli } from "./time.ts";
import type { Ordered } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

// Long enough that no group is skipped as stale, and the moq-lite grace for the one group the
// IETF case never produces.
const MAX_AGE = Milli(100);

async function session(protocol: string) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcast = origin.createBroadcast(Path.from("test"));
	broadcast.announce();
	const video = broadcast.createTrack("video");
	const remote = wireOf(client).consume(Path.from("test"));
	const reader = remote.track("video").subscribe({ maxAge: MAX_AGE }).ordered();

	return {
		video,
		reader,
		close: () => {
			broadcast.close();
			remote.close();
			client.close();
			server.close();
		},
	};
}

async function readAll(reader: Ordered): Promise<string[]> {
	const out: string[] = [];
	for (;;) {
		const next = await reader.readString();
		if (next === undefined) return out;
		out.push(next);
	}
}

// moq-lite carries the end in SUBSCRIBE_END as soon as it is declared, so a subscriber
// learns it while the last group is still to come.
test.each([Lite.ALPN_05, Lite.ALPN_06])(
	"%s: an end declared ahead of the live edge reaches the subscriber",
	async (alpn) => {
		const { video, reader, close } = await session(alpn);
		try {
			video.writeString("0");
			expect(await reader.readString()).toBe("0");
			video.writeString("1");
			expect(await reader.readString()).toBe("1");

			video.finishAt(3);
			expect(await reader.finished()).toBe(3);

			video.writeString("2");
			video.close();
			expect(await readAll(reader)).toEqual(["2"]);
			expect(await reader.closed).toBeNull();
			expect(reader.final()).toBe(3);
		} finally {
			close();
		}
	},
);

// moq-transport carries the end in an END_OF_TRACK object, so it survives a track that
// declared an end past the groups it produced, and the Stream Count lets the subscriber stop
// waiting for streams at once.
test.each([Ietf.ALPN.DRAFT_16, Ietf.ALPN.DRAFT_17, Ietf.ALPN.DRAFT_20])(
	"%s: END_OF_TRACK carries the declared end",
	async (alpn) => {
		const { video, reader, close } = await session(alpn);
		try {
			video.writeString("0");
			expect(await reader.readString()).toBe("0");

			video.finishAt(4);
			video.writeString("1");
			video.writeString("2");
			const ended = performance.now();
			video.close();

			expect(await readAll(reader)).toEqual(["1", "2"]);
			expect(await reader.closed).toBeNull();
			expect(reader.final()).toBe(4);
			// Every counted stream arrived, so nothing waited out the grace.
			expect(performance.now() - ended).toBeLessThan(TAIL_GRACE_MS);
		} finally {
			close();
		}
	},
);
