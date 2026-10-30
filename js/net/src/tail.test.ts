import { afterEach, describe, expect, test } from "bun:test";
import { type Installed, Instant, Manual, Milli, Nano, now } from "@moq/clock";
import { Once } from "@moq/signals";
import { accept, connect } from "./connection/index.ts";
import * as Ietf from "./ietf/index.ts";
import { GRACE as TAIL_GRACE_MS } from "./ledger.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, textFrame } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Tail } from "./tail.ts";
import { Timescale } from "./time.ts";
import type { Ordered } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

// Long enough that no group is skipped as stale, and the moq-lite grace for the one group the
// IETF case never produces.
const MAX_DELAY = Milli(100);

async function session(protocol: string) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcastPublication = origin.createBroadcast(Path.from("test"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const remote = wireOf(client).consume(Path.from("test"));
	const reader = remote.track("video").subscribe({ maxDelay: MAX_DELAY }).ordered();

	return {
		video,
		reader,
		close: () => {
			broadcast.close();
			remote.close();
			client.abort();
			server.abort();
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
			video.writeFrame(textFrame("0"));
			expect(await reader.readString()).toBe("0");
			video.writeFrame(textFrame("1"));
			expect(await reader.readString()).toBe("1");

			video.finishAt({ group: 3, frame: 0 });
			expect(await reader.finished()).toEqual({ group: 3, frame: 0 });

			video.writeFrame(textFrame("2"));
			video.finish();
			video.close();
			expect(await readAll(reader)).toEqual(["2"]);
			expect(await reader.closed).toBeNull();
			expect(await reader.finished()).toEqual({ group: 3, frame: 0 });
		} finally {
			close();
		}
	},
);

// moq-transport carries the end in an END_OF_TRACK object, so it survives a track that
// declared an end past the groups it produced, and the Stream Count lets the subscriber stop
// waiting for streams at once.
test.each([Ietf.ALPN.DRAFT_22])("%s: END_OF_TRACK carries the declared end", async (alpn) => {
	const { video, reader, close } = await session(alpn);
	try {
		video.writeFrame(textFrame("0"));
		expect(await reader.readString()).toBe("0");

		video.finishAt({ group: 4, frame: 0 });
		video.writeFrame(textFrame("1"));
		video.writeFrame(textFrame("2"));
		const ended = now();
		video.finish();
		video.close();

		expect(await readAll(reader)).toEqual(["1", "2"]);
		expect(await reader.closed).toBeNull();
		expect(await reader.finished()).toEqual({ group: 4, frame: 0 });
		// Every counted stream arrived, so nothing waited out the grace.
		expect(Nano.toMilli(Instant.durationSince(now(), ended))).toBeLessThan(TAIL_GRACE_MS);
	} finally {
		close();
	}
});

describe("Tail", () => {
	const GRACE = Milli(1000);

	let installed: Installed | undefined;
	afterEach(() => installed?.close());

	// A tail on a manual clock, with `at(ms)` moving the clock to an absolute time.
	function tail(grace = GRACE) {
		const clock = new Manual();
		installed = clock.install();
		const at = (ms: number) => clock.jump(Nano(Nano.fromMilli(Milli(ms)) - clock.now()));
		return { at, clock, tail: new Tail({ grace: () => grace }) };
	}

	test("a gap older than the grace folds away", () => {
		const { at, tail: t } = tail();
		t.account(0, 1);
		t.account(2, 3);
		at(GRACE / 2);
		t.account(4, 5);
		expect(t.covers(0, 3)).toBe(false);

		// Splitting the younger gap keeps it as late as it was.
		t.account(6, 7);
		at(GRACE);
		t.account(8, 9);
		expect(t.covers(0, 3)).toBe(true);
		expect(t.covers(0, 5)).toBe(false);
	});

	test("splitting a gap leaves both halves with its age", () => {
		const { at, tail: t } = tail();
		t.account(0, 1);
		t.account(9, 10);
		at(GRACE / 2);
		t.account(5, 6);
		expect(t.covers(0, 10)).toBe(false);

		at(GRACE);
		t.account(9, 10);
		expect(t.covers(0, 10)).toBe(true);
	});

	test("a lowered floor restarts the gap age", () => {
		const { at, tail: t } = tail();
		t.account(3, 4);
		t.account(9, 10);

		at(GRACE * 2);
		t.demand(1, 3);
		t.account(1, 2);
		expect(t.covers(1, 4)).toBe(false);
		// Only the gap the floor reached restarts.
		expect(t.covers(3, 10)).toBe(true);

		at(GRACE * 3);
		t.account(9, 10);
		expect(t.covers(1, 10)).toBe(true);
	});

	test("the grace waits for a stream being read", async () => {
		const { clock, tail: t } = tail(Milli(10));
		const read = t.open(0);
		let settled = false;
		const settling = t
			.settle(() => false, new Once<unknown>())
			.then(() => {
				settled = true;
			});

		await clock.advance(Nano.fromMilli(Milli(50)));
		expect(settled).toBe(false);
		read();
		await settling;
	});
});
