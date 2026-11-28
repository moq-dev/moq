import { afterEach, describe, expect, test } from "bun:test";
import { Duration, type Installed, Instant, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Once } from "@moq/signals";
import { accept, connect } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import * as Ietf from "./ietf/index.ts";
import { NO_MAX_DELAY } from "./internal.ts";
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

	// When a stream last delivered data, which the test advances.
	const meter = () => {
		let last = now();
		return {
			read: () => last,
			deliver: () => {
				last = now();
			},
		};
	};

	const SILENCE = TAIL_GRACE_MS;

	test("a drain gives up on a stream that delivered nothing for the grace", async () => {
		const { clock, tail: t } = tail(Duration.millis(10));
		const quiet = meter();
		const read = t.open(0n, quiet.read);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		let settled = false;
		const draining = t
			.drain(() => false, new Once<unknown>())
			.then(() => {
				settled = true;
			});

		// Its own grace is no measure: only silence for GRACE is.
		await clock.advance(SILENCE.div(2));
		expect(stalled).toBe(false);
		await clock.advance(SILENCE.div(2));
		await read.stalled;
		// Still counted until its reader lets go, which the drain waits for.
		expect(t.reading).toBe(true);
		expect(settled).toBe(false);
		read.done();
		await draining;
	});

	test("a drain with no age limit still gives up on a silent stream", async () => {
		const { clock, tail: t } = tail(Duration.millis(NO_MAX_DELAY));
		const read = t.open(0n, meter().read);
		const draining = t.drain(() => true, new Once<unknown>());

		await clock.advance(SILENCE);
		await read.stalled;
		read.done();
		await draining;
	});

	test("a stream still delivering within each grace is waited for", async () => {
		const { clock, tail: t } = tail();
		const slow = meter();
		const read = t.open(0n, slow.read);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		const draining = t.drain(() => false, new Once<unknown>());

		for (let i = 0; i < 10; i++) {
			await clock.advance(SILENCE.mul(3).div(4));
			slow.deliver();
		}
		expect(stalled).toBe(false);

		await clock.advance(SILENCE);
		await read.stalled;
		read.done();
		await draining;
	});

	test("a drain gives a stream the whole grace after it starts, however long it was quiet before", async () => {
		const { clock, tail: t } = tail();
		const read = t.open(0n, meter().read);
		await clock.advance(SILENCE.mul(5));
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		const draining = t.drain(() => false, new Once<unknown>());

		await clock.advance(SILENCE.mul(9).div(10));
		expect(stalled).toBe(false);
		await clock.advance(SILENCE.div(10));
		await read.stalled;
		read.done();
		await draining;
	});

	test("a drain's limit still caps the wait for groups that never arrive", async () => {
		const { clock, tail: t } = tail(Duration.millis(NO_MAX_DELAY));
		const draining = t.drain(() => false, new Once<unknown>(), Duration.millis(10));

		await clock.advance(Duration.millis(10));
		await draining;
	});

	test("a settle before the end never gives up on a quiet stream", async () => {
		const { clock, tail: t } = tail(Duration.millis(10));
		const read = t.open(0n, meter().read);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		const settling = t.settle(() => false, new Once<unknown>());

		await clock.advance(SILENCE.mul(10));
		expect(stalled).toBe(false);
		read.done();
		await settling;
	});

	test("a parked stream is not given up on, and restarts its quiet time when it resumes", async () => {
		const { clock, tail: t } = tail();
		const read = t.open(0n, meter().read);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		// Something else keeps the drain waiting while it is parked.
		const release = t.hold();
		const draining = t.drain(() => false, new Once<unknown>());

		read.park();
		await clock.advance(SILENCE.mul(5));
		expect(stalled).toBe(false);

		read.resume();
		await clock.advance(SILENCE.div(2));
		expect(stalled).toBe(false);
		await clock.advance(SILENCE.div(2));
		await read.stalled;
		read.done();
		release();
		await draining;
	});

	test("a stream opened without a meter is waited for until it ends", async () => {
		const { clock, tail: t } = tail();
		const read = t.open(0n);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		let settled = false;
		const draining = t
			.drain(() => false, new Once<unknown>())
			.then(() => {
				settled = true;
			});

		await clock.advance(SILENCE.mul(10));
		expect(stalled).toBe(false);
		expect(settled).toBe(false);
		read.done();
		await draining;
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
		const stream = t.open(0n, meter().read);
		let stalled = false;
		void stream.stalled.then(() => {
			stalled = true;
		});
		let settled = false;
		const settling = t
			.settle(() => true, new Once<unknown>(), Duration.millis(10))
			.then(() => {
				settled = true;
			});

		await clock.advance(GRACE.mul(100));
		expect(settled).toBe(false);
		expect(stalled).toBe(false);
		stream.done();
		await settling;
	});

	test("with no grace, a drain never gives up on a silent stream", async () => {
		const { clock, tail: t } = tail(null);
		const read = t.open(0n, meter().read);
		let stalled = false;
		void read.stalled.then(() => {
			stalled = true;
		});
		const draining = t.drain(() => true, new Once<unknown>());

		await clock.advance(SILENCE.mul(100));
		expect(stalled).toBe(false);
		read.done();
		await draining;
	});

	test("a parked stream holds nothing until it resumes", () => {
		const { tail: t } = tail(Duration.millis(10));
		const stream = t.open(0n, meter().read);
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
