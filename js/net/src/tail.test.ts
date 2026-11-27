import { afterEach, describe, expect, test } from "bun:test";
import { Duration, type Installed, Instant } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Once } from "@moq/signals";
import { accept, connect } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import * as Ietf from "./ietf/index.ts";
import { GRACE as TAIL_GRACE_MS } from "./ledger.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair, manual, textFrame } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { withMax } from "./subscription.test-util.ts";
import { Tail } from "./tail.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Ordered } from "./track.ts";
import { wireOf } from "./wire.ts";

const url = new URL("https://localhost:4443/test");

// Long enough that no group is skipped as stale, and the moq-lite grace for the one group the
// IETF case never produces.
const MAX_DELAY = Duration.millis(100);

async function session(protocol: string) {
	const pair = createMockTransportPair(protocol);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connect({ url, transport: pair.client }),
		accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const broadcastRoute = origin.route(Path.from("test"));
	const broadcast = broadcastRoute.broadcast;
	const video = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const remote = wireOf(client).consume(Path.from("test"));
	const reader = remote
		.track("video")
		.subscribe(withMax({ delay: { max: MAX_DELAY } }))
		.ordered();

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
			video.writeFrame(textFrame("0", Timestamp.fromMillis(0)));
			expect(await reader.readString()).toBe("0");
			video.writeFrame(textFrame("1", Timestamp.fromMillis(1)));
			expect(await reader.readString()).toBe("1");

			video.finishAt({ group: 3n, frame: 0 });
			expect(await reader.finished()).toEqual({ group: 3n, frame: 0 });

			video.writeFrame(textFrame("2", Timestamp.fromMillis(2)));
			video.finish();
			video.close();
			expect(await readAll(reader)).toEqual(["2"]);
			expect(await reader.closed).toBeNull();
			expect(await reader.finished()).toEqual({ group: 3n, frame: 0 });
		} finally {
			close();
		}
	},
);

// moq-transport carries the end in an END_OF_TRACK object, so it survives a track that
// declared an end past the groups it produced, and the Stream Count lets the subscriber stop
// waiting for streams at once.
test.each([Ietf.ALPN.DRAFT_22])("%s: END_OF_TRACK carries the declared end", async (alpn) => {
	using clock = manual();
	const { video, reader, close } = await session(alpn);
	try {
		video.writeFrame(textFrame("0", Timestamp.fromMillis(0)));
		expect(await reader.readString()).toBe("0");

		video.finishAt({ group: 4n, frame: 0 });
		video.writeFrame(textFrame("1", Timestamp.fromMillis(1)));
		video.writeFrame(textFrame("2", Timestamp.fromMillis(2)));
		const ended = clock.now();
		video.finish();
		video.close();

		const read = readAll(reader);
		await clock.drive(reader.closed);
		// Every counted stream arrived, so nothing waited out the grace.
		expect(clock.now().since(ended).lt(TAIL_GRACE_MS)).toBe(true);
		expect(await read).toEqual(["1", "2"]);
		expect(await reader.closed).toBeNull();
		expect(await reader.finished()).toEqual({ group: 4n, frame: 0 });
	} finally {
		close();
	}
});

describe("Tail", () => {
	const GRACE = Duration.millis(1000);

	let installed: Installed | undefined;
	afterEach(() => installed?.close());

	// A tail on a manual clock, with `at(ms)` moving the clock to an absolute time. A `null` grace
	// is lite-07's, which waits for the wire to account for every start.
	function tail(grace: Duration | null = GRACE) {
		const clock = new Manual();
		installed = clock.install();
		const at = (to: Duration) => clock.jump(Instant.ZERO.saturatingAdd(to).since(clock.now()));
		return { at, clock, tail: new Tail({ grace: () => grace ?? undefined }) };
	}

	test("a gap older than the grace folds away", () => {
		const { at, tail: t } = tail();
		t.account(0n, 1n);
		t.account(2n, 3n);
		at(GRACE.div(2));
		t.account(4n, 5n);
		expect(t.covers(0n, 3n)).toBe(false);

		// Splitting the younger gap keeps it as late as it was.
		t.account(6n, 7n);
		at(GRACE);
		t.account(8n, 9n);
		expect(t.covers(0n, 3n)).toBe(true);
		expect(t.covers(0n, 5n)).toBe(false);
	});

	test("splitting a gap leaves both halves with its age", () => {
		const { at, tail: t } = tail();
		t.account(0n, 1n);
		t.account(9n, 10n);
		at(GRACE.div(2));
		t.account(5n, 6n);
		expect(t.covers(0n, 10n)).toBe(false);

		at(GRACE);
		t.account(9n, 10n);
		expect(t.covers(0n, 10n)).toBe(true);
	});

	test("a lowered floor restarts the gap age", () => {
		const { at, tail: t } = tail();
		t.account(3n, 4n);
		t.account(9n, 10n);

		at(GRACE.mul(2));
		t.demand(1n, 3n);
		t.account(1n, 2n);
		expect(t.covers(1n, 4n)).toBe(false);
		// Only the gap the floor reached restarts.
		expect(t.covers(3n, 10n)).toBe(true);

		at(GRACE.mul(3));
		t.account(9n, 10n);
		expect(t.covers(1n, 10n)).toBe(true);
	});

	test("the grace waits for a stream being read", async () => {
		const { clock, tail: t } = tail(Duration.millis(10));
		const read = t.open(0n);
		let settled = false;
		const settling = t
			.settle(() => false, new Once<unknown>())
			.then(() => {
				settled = true;
			});

		await clock.advance(Duration.millis(50));
		expect(settled).toBe(false);
		read.done();
		await settling;
	});

	test("with no grace, a gap is owed until it is accounted for", async () => {
		const { at, clock, tail: t } = tail(null);
		t.account(0n, 1n);
		t.account(2n, 3n);
		let settled = false;
		const settling = t
			.settle(() => t.covers(0n, 3n), new Once<unknown>())
			.then(() => {
				settled = true;
			});

		at(GRACE.mul(100));
		t.account(4n, 5n);
		await clock.advance(GRACE.mul(100));
		expect(t.covers(0n, 3n)).toBe(false);
		expect(settled).toBe(false);

		t.account(1n, 2n);
		await settling;
	});

	// Lite-07 times nothing: a limit caps only a grace, so a stream is waited for until its read ends.
	test("with no grace, a stream being read holds the end past any limit", async () => {
		const { clock, tail: t } = tail(null);
		const stream = t.open(0n);
		let settled = false;
		const settling = t
			.settle(() => true, new Once<unknown>(), Duration.millis(10))
			.then(() => {
				settled = true;
			});

		await clock.advance(GRACE.mul(100));
		expect(settled).toBe(false);
		stream.done();
		await settling;
	});

	test("a parked stream holds nothing until it resumes", () => {
		const { tail: t } = tail(Duration.millis(10));
		const stream = t.open(0n);
		stream.park();
		stream.park();
		expect(t.reading).toBe(false);
		stream.resume();
		stream.resume();
		expect(t.reading).toBe(true);
		stream.done();
		expect(t.reading).toBe(false);
	});
});
