// A received copy asks its publisher for the groups its readers miss, over lite FETCH, as the Rust
// lite subscriber's `FillServeRun` does, or for more than one group, as its `RangeFills` does:
// on lite-07 through its subscription's ranges, and on a SUBSCRIBE of its own for what that
// subscription already delivered, as its `Recovery` does, and before lite-07 on a SUBSCRIBE of its
// own per run: below where the copy's feed started, by the copy's start rule, and inside the copy,
// once it evicted what it received.
import { expect, spyOn, test } from "bun:test";
import { Duration, Elapsed, sleep, timeout } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { DEFAULT_EXPIRY } from "../cache.ts";
import { accept as acceptSession, connect as connectSession } from "../connection/index.ts";
import { defined } from "../defined.test-util.ts";
import { StreamCode, StreamError } from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { WireRoute } from "../hop.ts";
import * as Instance from "../instance.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import { lines } from "../open.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { withMax } from "../subscription.test-util.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { FillGroups, Producer, Subscriber, Subscription } from "../track.ts";
import { wireOf } from "../wire.ts";
import * as Lite from "./index.ts";
import { Publisher as LitePublisher } from "./publisher.ts";
import { Subscriber as LiteSubscriber } from "./subscriber.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();
const url = new URL("https://localhost:4443/test");

// A JS publisher's track and a JS subscriber's session to it. The publisher's archive serves
// every group its cache no longer holds, except group 1, which it never had, or with `timed`, none.
// `answered`, when given, holds the publisher's archive until it settles.
async function peers(alpn: string, timed = false, answered?: Promise<unknown>) {
	const pair = createMockTransportPair(alpn);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("room"));
	const broadcast = publication.broadcast;
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			await answered;
			for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
				if (key === 1n) {
					fill.dropGroups(1n, 2n);
					continue;
				}
				// The groups of `populateTimed` start on whole seconds and stay cached, so a lookup
				// asks only for times between them, where none starts.
				if (timed) {
					fill.dropGroups(key, (key / 1_000n + 1n) * 1_000n);
					continue;
				}
				write(defined(fill.accept(key)), key, "old");
			}
			fill.close();
		}
	})();
	const remote = wireOf(client).consume(Path.from("room"));
	// Every FETCH the copy sends opens a bidi stream of its own.
	const fetches = () => pair.client.sendStreams.bidi.length;
	return {
		track,
		remote,
		fetches,
		// The publisher's end of the transport, for steering delivery.
		server: pair.server,
		close() {
			remote.close();
			requests.close();
			broadcast.close();
			client.abort();
			server.abort();
			origin.close();
		},
	};
}

// Group `sequence` presents at `sequence` seconds, with its origin in its payload.
function write(group: ReturnType<Producer["createGroup"]>, key: bigint, kind: string): void {
	group.writeFrame({
		payload: enc.encode(`${kind} ${key}`),
		timestamp: Timestamp.fromMillis(Number(key) * 1000),
	});
	group.finish();
}

async function payload(group: GroupConsumer): Promise<string> {
	const frame = await group.readFrame();
	group.close();
	return dec.decode(frame?.payload);
}

// Read `count` groups from `reader`, by sequence.
async function take(reader: Subscriber, count: number): Promise<Map<bigint, string>> {
	const got = new Map<bigint, string>();
	while (got.size < count) {
		const group = await reader.recvGroup();
		if (!group) break;
		got.set(group.start, await payload(group));
	}
	return got;
}

// Whether `reader` hands out no group once every pending task has settled: `clock` fires the
// deadline only after draining them.
async function idle(clock: Manual, reader: Subscriber): Promise<boolean> {
	try {
		(await clock.drive(timeout(Duration.millis(200), reader.recvGroup())))?.close();
		return false;
	} catch (err) {
		if (err instanceof Elapsed) return true;
		throw err;
	}
}

// Groups 0 to 4, one per second: a group is as old as the start of the one after it, so a
// reader with a half-second budget starts at group 3, on lite-06 and later.
const NARROW: Subscription = withMax({ delay: { max: Duration.millis(500) } });
const WIDE: Subscription = withMax({ delay: { max: null }, order: "ascending" });

function populate(track: Producer): void {
	for (let key = 0n; key < 5n; key++) write(track.createGroup(key), key, "live");
}

// What `take` read, in sequence order.
function sorted(got: Map<bigint, string>): [bigint, string][] {
	return [...got].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}

const ALL = [0n, 1n, 2n, 3n, 4n].map((key): [bigint, string] => [key, `live ${key}`]);

test("copy fill: lite-07 fills below a start resolved from the budget, as far as the reader's own budget reaches", async () => {
	const peer = await peers(Lite.ALPN_07_WIP);
	try {
		populate(peer.track);
		const narrow = peer.remote.track("video").subscribe(NARROW);
		expect((await narrow.recvGroup())?.start).toBe(3n);
		const before = peer.fetches();

		// The copy cannot tell which budget picked its start, so it fills what this one reaches,
		// through its subscription's ranges rather than a stream of its own.
		const wide = peer.remote.track("video").subscribe(WIDE);
		expect(sorted(await take(wide, 5))).toEqual(ALL);
		expect(peer.fetches()).toBe(before);
		narrow.close();
		wide.close();
	} finally {
		peer.close();
	}
});

// A live join `ms` behind the newest frame, read in order.
function joining(ms: number): Subscription {
	return { order: "ascending", ranges: [{ start: { live: { min: Duration.millis(ms), max: null } } }] };
}

// Groups starting at 0 to 4 seconds, each keyed by the millisecond it starts at, as a timed
// track's groups are: a live join measures back from the newest in the same time.
function populateTimed(track: Producer): void {
	for (let key = 0n; key < 5_000n; key += 1_000n) {
		const group = track.createGroup(key);
		group.writeFrame({ payload: enc.encode(`live ${key}`), timestamp: Timestamp.fromMillis(Number(key)) });
		group.finish();
	}
}

// What `populateTimed` wrote from `from` milliseconds on.
function timed(from: bigint): [bigint, string][] {
	return [0n, 1_000n, 2_000n, 3_000n, 4_000n].filter((key) => key >= from).map((key) => [key, `live ${key}`]);
}

// The copy's upstream subscription asks for the oldest group, since one reader starts there, yet
// each reader starts where it would on the origin.
test("copy fill: lite-07 starts an oldest reader and a live join on one copy where each asked", async () => {
	const peer = await peers(Lite.ALPN_07_WIP, true);
	try {
		populateTimed(peer.track);
		const live = peer.remote.track("video").subscribe(joining(1500));
		const oldest = peer.remote.track("video").subscribe(WIDE);
		// 1.5 s behind the newest frame at 4 s is 2.5 s, which the group from 2 s holds.
		expect(sorted(await take(live, 3))).toEqual(timed(2_000n));
		expect(sorted(await take(oldest, 5))).toEqual(timed(0n));
		const origin = peer.track.subscribe(joining(1500));
		expect(sorted(await take(origin, 3))).toEqual(timed(2_000n));
		live.close();
		oldest.close();
		origin.close();
	} finally {
		peer.close();
	}
});

// A live join reaching back past what the copy holds can't be proven from its groups, so the copy
// asks its upstream with a range of that lag, and the reader starts where the origin does.
test("copy fill: lite-07 asks upstream for a live join reaching back past what the copy holds", async () => {
	const peer = await peers(Lite.ALPN_07_WIP, true);
	try {
		populateTimed(peer.track);
		// 0.5 s behind the newest frame is 3.5 s, in the group from 3 s, so the copy holds that and
		// the next.
		const near = peer.remote.track("video").subscribe(joining(500));
		expect(sorted(await take(near, 2))).toEqual(timed(3_000n));

		// 2.5 s behind is 1.5 s, in the group from 1 s, which the copy never received.
		const far = peer.remote.track("video").subscribe(joining(2500));
		expect(sorted(await take(far, 4))).toEqual(timed(1_000n));
		const asked = peer.track.subscription.peek()?.ranges ?? [];
		expect(asked.some((range) => typeof range.start === "object" && range.start.live.min?.asMillis() === 2500)).toBe(
			true,
		);
		const origin = peer.track.subscribe(joining(2500));
		expect(sorted(await take(origin, 4))).toEqual(timed(1_000n));
		near.close();
		far.close();
		origin.close();
	} finally {
		peer.close();
	}
});

// A live join whose range ends before the group it joins at reads nothing and ends, on the copy as
// at the origin: the publisher answers the copy's range empty, which says the join starts past its
// end. Mirrors the Rust `a_join_ending_before_its_start_ends_empty_through_a_relay`.
test("copy fill: lite-07 ends a live join answered empty", async () => {
	const peer = await peers(Lite.ALPN_07_WIP, true);
	try {
		populateTimed(peer.track);
		// 1 s behind the newest frame at 4 s is 3 s, past the range's end at 1 s.
		const bounded = (): Subscription => ({
			...joining(1000),
			ranges: [{ start: { live: { min: Duration.millis(1000), max: null } }, end: 1_000n }],
		});
		const origin = peer.track.subscribe(bounded());
		expect(sorted(await take(origin, 1))).toEqual([]);
		const relayed = peer.remote.track("video").subscribe(bounded());
		expect(sorted(await take(relayed, 1))).toEqual([]);
		// That empty answer places no reader whose range reaches past its end: one unbounded, with the
		// same Min Age, joins where the origin does.
		const open = peer.remote.track("video").subscribe(joining(1000));
		expect(sorted(await take(open, 2))).toEqual(timed(3_000n));
		origin.close();
		relayed.close();
		open.close();
	} finally {
		peer.close();
	}
});

test("copy fill: lite-06 asks nothing below a start resolved from the budget, and relies on the re-offer", async () => {
	const peer = await peers(Lite.ALPN_06);
	try {
		populate(peer.track);
		const narrow = peer.remote.track("video").subscribe(NARROW);
		expect((await narrow.recvGroup())?.start).toBe(3n);
		const before = peer.fetches();

		// The widened budget has the publisher re-offer them: lite-06 FETCH carries no budget.
		const wide = peer.remote.track("video").subscribe(WIDE);
		expect(sorted(await take(wide, 5))).toEqual(ALL);
		expect(peer.fetches()).toBe(before);
		narrow.close();
		wide.close();
	} finally {
		peer.close();
	}
});

test("copy fill: lite-05 fills below the live edge only what a reader's ranges name", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const peer = await peers(Lite.ALPN_05);
	try {
		populate(peer.track);
		const live = peer.remote.track("video").subscribe(NARROW);
		expect((await live.recvGroup())?.start).toBe(4n);
		const before = peer.fetches();

		// Reaching back to the oldest group names no floor, so the live edge bounds it.
		const oldest = peer.remote.track("video").subscribe(WIDE);
		expect((await oldest.recvGroup())?.start).toBe(4n);
		expect(await idle(clock, oldest)).toBe(true);
		expect(peer.fetches()).toBe(before);

		// A floor the ranges name is filled.
		const named = peer.remote.track("video").subscribe({ ...WIDE, ranges: [{ start: 0n }] });
		expect(sorted(await take(named, 5))).toEqual(ALL);
		expect(peer.fetches()).toBeGreaterThan(before);
		live.close();
		oldest.close();
		named.close();
	} finally {
		peer.close();
		installed.close();
	}
});

test.each([Lite.ALPN_05, Lite.ALPN_06, Lite.ALPN_07_WIP])(
	"copy fill: %s fills what the copy evicted from the publisher's archive",
	async (alpn) => {
		const clock = new Manual();
		const installed = clock.install();
		const peer = await peers(alpn);
		try {
			const history: Subscription = withMax({
				ranges: [{ start: 0n }],
				delay: { max: null },
				order: "ascending",
			});
			const first = peer.remote.track("video").subscribe(history);
			for (let key = 0n; key < 4n; key++) write(peer.track.createGroup(key), key, "live");
			// Before lite-07 a subscription carries no order and is served newest first, so only
			// the set of groups is fixed.
			expect(sorted(await take(first, 4)).map(([key]) => key)).toEqual([0n, 1n, 2n, 3n]);

			// Both caches age out what nobody touched, a prune slice past the window; only the
			// live edge stays.
			await clock.advance(DEFAULT_EXPIRY.add(Duration.millis(10_000)));
			write(peer.track.createGroup(4n), 4n, "live");
			expect((await first.recvGroup())?.start).toBe(4n);
			const before = peer.fetches();

			// The same ranges, so nothing changes upstream: only a fill brings what the copy lost, on
			// lite-07 one SUBSCRIBE for the run, its groups and its miss. Before lite-07 a SUBSCRIBE
			// waits only for the group it starts at, so at most one per group the run holds: how many
			// depends on what lands before its range is capped there.
			const again = peer.remote.track("video").subscribe(history);
			const got = await take(again, 4);
			expect(sorted(got)).toEqual([
				[0n, "old 0"],
				[2n, "old 2"],
				[3n, "old 3"],
				[4n, "live 4"],
			]);
			if (alpn === Lite.ALPN_07_WIP) expect(peer.fetches()).toBe(before + 1);
			else expect(peer.fetches()).toBeLessThanOrEqual(before + 3);
			first.close();
			again.close();
		} finally {
			peer.close();
			installed.close();
		}
	},
);

// The publisher finishes the track while a lite-05/06 copy still recovers a run it evicted, on a
// subscription of its own: the copy's subscription FINs, but the copy stays open until upstream
// answered the run, even once that run's FIN beats its group streams, so the reader still gets it.
// Mirrors the Rust
// `lite06_fill_walk::a_recovery_outlives_the_finished_subscription`.
test.each([Lite.ALPN_05, Lite.ALPN_06])("copy fill: %s recovers a run past the track's finish", async (alpn) => {
	const clock = new Manual();
	const installed = clock.install();
	let answer = () => {};
	const peer = await peers(
		alpn,
		false,
		new Promise<void>((resolve) => {
			answer = resolve;
		}),
	);
	try {
		const history: Subscription = { ranges: [{ start: 0n }], order: "ascending" };
		const first = peer.remote.track("video").subscribe(history);
		for (let key = 0n; key < 4n; key++) write(peer.track.createGroup(key), key, "live");
		expect(sorted(await take(first, 4)).map(([key]) => key)).toEqual([0n, 1n, 2n, 3n]);

		// Both caches age out what nobody touched; only the live edge stays.
		await clock.advance(DEFAULT_EXPIRY.add(Duration.millis(10_000)));
		write(peer.track.createGroup(4n), 4n, "live");
		expect((await first.recvGroup())?.start).toBe(4n);

		// The run goes out on a recovery the archive holds, and the publisher ends the track.
		const again = peer.remote.track("video").subscribe({ ...history, ranges: [{ start: 2n, end: 3n }] });
		const got = take(again, 2);
		await clock.advance(Duration.millis(100));
		peer.track.finish();
		await clock.advance(Duration.secs(60));

		// Upstream answers and, the track being over, FINs the run's subscription before its group
		// streams land, as QUIC may deliver them, within the grace.
		peer.server.holdUnis();
		answer();
		await clock.advance(Duration.millis(500));
		while (peer.server.heldUnis() > 0) peer.server.releaseOldestUni();
		expect(sorted(await got)).toEqual([
			[2n, "old 2"],
			[3n, "old 3"],
		]);
		first.close();
		again.close();
	} finally {
		peer.close();
		installed.close();
	}
});

// Resolve once nobody reads `group` any more.
async function unwanted(group: { demand(): { wanted: { peek(): boolean; changed(): Promise<boolean> } } }) {
	const wanted = group.demand().wanted;
	while (wanted.peek()) await wanted.changed();
}

test("copy fill: a filled group stops downloading once every reader left or passed it, read again or not", async () => {
	const pair = createMockTransportPair(Lite.ALPN_07_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("room"));
	const track = defined(publication.broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	write(track.createGroup(1n), 1n, "live");
	// The archive answers group 0 with one frame and keeps it open.
	const requests = track.requests();
	const fills: FillGroups[] = [];
	const archived = (async () => {
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		fills.push(fill);
		const group = defined(fill.accept(0n));
		group.writeFrame({ payload: enc.encode("old 0"), timestamp: Timestamp.fromMillis(0) });
		return group;
	})();
	const remote = wireOf(client).consume(Path.from("room"));
	try {
		// A live reader keeps the copy up, and has read past group 0 before it lands. It never
		// reads again, so the copy it still buffers is all that is left of it.
		const reader = remote
			.track("video")
			.subscribe({ ranges: [{ start: { live: { min: Duration.ZERO, max: Duration.ZERO } } }] });
		expect((await reader.ordered().nextGroup())?.start).toBe(1n);
		const history = remote.track("video").subscribe({ ...WIDE, ranges: [{ start: 0n, end: 0n }] });
		const group = await history.recvGroup();
		expect(group?.start).toBe(0n);
		expect(dec.decode((await group?.readFrame())?.payload)).toBe("old 0");
		const source = await archived;

		// A reader that has yet to reach the group keeps it downloading once the first leaves.
		const behind = remote.track("video").subscribe({ ...WIDE, ranges: [{ start: 0n, end: 0n }] });
		group?.close();
		history.close();
		source.writeFrame({ payload: enc.encode("old 0 more"), timestamp: Timestamp.fromMillis(1) });
		const reached = await behind.recvGroup();
		expect(reached?.start).toBe(0n);
		expect(dec.decode((await reached?.readFrame())?.payload)).toBe("old 0");
		expect(dec.decode((await reached?.readFrame())?.payload)).toBe("old 0 more");

		// Once it leaves too, the copy cancels its FETCH, and the publisher's reader goes with it.
		reached?.close();
		behind.close();
		await unwanted(source);
		expect(source.closed.peek()).toBeUndefined();
		source.abort(new Error("done"));
		reader.close();
	} finally {
		for (const fill of fills) fill.close();
		remote.close();
		requests.close();
		publication.broadcast.close();
		client.abort();
		server.abort();
		origin.close();
	}
});

test("copy fill: a filled group nobody holds any more stops downloading", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(Lite.ALPN_07_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("room"));
	const track = defined(publication.broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	write(track.createGroup(1n), 1n, "live");
	// The archive answers group 0 with one frame and never finishes it.
	const requests = track.requests();
	const fills: FillGroups[] = [];
	const archived = (async () => {
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		fills.push(fill);
		const group = defined(fill.accept(0n));
		group.writeFrame({ payload: enc.encode("old 0"), timestamp: Timestamp.fromMillis(0) });
		return group;
	})();
	const remote = wireOf(client).consume(Path.from("room"));
	try {
		// A live reader keeps the copy up. It reads in order, so it lets go of what it passes.
		const reader = remote
			.track("video")
			.subscribe({ ranges: [{ start: { live: { min: Duration.ZERO, max: Duration.ZERO } } }] });
		const live = reader.ordered();
		expect((await live.nextGroup())?.start).toBe(1n);
		const history = remote.track("video").subscribe({ ...WIDE, ranges: [{ start: 0n, end: 0n }] });
		const group = await history.recvGroup();
		expect(group?.start).toBe(0n);
		expect(dec.decode((await group?.readFrame())?.payload)).toBe("old 0");
		const source = await archived;
		expect(source.demand().wanted.peek()).toBeTruthy();

		// The history reader leaves mid-group, and the live one reads past it: nobody holds the
		// group any more, so the copy cancels its FETCH and aborts the truncated group rather
		// than keep downloading it. The publisher may close the group's stream first, once the
		// narrower budget left it stale, which leaves the copy a closed group: either way the
		// source loses its last reader.
		group?.close();
		history.close();
		write(track.createGroup(2n), 2n, "live");
		expect((await live.nextGroup())?.start).toBe(2n);
		await clock.drive(
			timeout(
				Duration.millis(2000),
				(async () => {
					while (source.demand().wanted.peek()) await sleep(Duration.millis(10));
				})(),
			),
		);
		source.abort(new Error("done"));
		live.close();
	} finally {
		for (const fill of fills) fill.close();
		remote.close();
		requests.close();
		publication.broadcast.close();
		client.abort();
		server.abort();
		origin.close();
		installed.close();
	}
});

// Lite-07: a filled group whose stream the session reset to make room ends with TOO_FAR_BEHIND, which
// the drop on the copy's subscription, which the fill rides, names too.
test("copy fill: a group reset to make room ends", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(Lite.ALPN_07_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("room"));
	const track = defined(publication.broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	write(track.createGroup(2n), 2n, "live");
	// The archive answers group 0 with one frame and keeps it open, so its stream is still open, and
	// group 1 whole.
	const requests = track.requests();
	const fills: FillGroups[] = [];
	const archived = (async () => {
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		fills.push(fill);
		const group = defined(fill.accept(0n));
		group.writeFrame({ payload: enc.encode("old 0"), timestamp: Timestamp.fromMillis(0) });
		write(defined(fill.accept(1n)), 1n, "old");
		return group;
	})();
	const remote = wireOf(client).consume(Path.from("room"));
	try {
		const reader = remote
			.track("video")
			.subscribe({ ranges: [{ start: { live: { min: Duration.ZERO, max: Duration.ZERO } } }] });
		const live = await reader.ordered().nextGroup();
		expect(live?.start).toBe(2n);
		expect(dec.decode((await live?.readFrame())?.payload)).toBe("live 2");
		expect(await live?.readFrame()).toBeUndefined();
		live?.close();
		// Two groups, so the fill rides the subscription's ranges.
		const history = remote.track("video").subscribe({ ...WIDE, ranges: [{ start: 0n, end: 1n }] });
		const group = await history.recvGroup();
		expect(group?.start).toBe(0n);
		expect(dec.decode((await group?.readFrame())?.payload)).toBe("old 0");
		await archived;

		// The session resets the group's stream to make room, so the publisher drops it on the
		// subscription the fill rides.
		const one = await history.recvGroup();
		expect(one?.start).toBe(1n);
		while (await one?.readFrame());
		// The live stream has finished, so the archived group's is the only one still open.
		await clock.advance(Duration.ZERO);
		const open = lines(pair.server).streams.list();
		expect(open.length).toBe(1);
		open[0]?.preempt();

		// Within the deadline, which fires once every pending task settled.
		const ended = await clock
			.drive(timeout(Duration.millis(1000), Promise.resolve(group?.closed)))
			.catch((err: unknown) => err);
		expect(ended).toBeInstanceOf(StreamError);
		expect((ended as StreamError).code).toBe(StreamCode.TooFarBehind);
		history.close();
		reader.close();
	} finally {
		for (const fill of fills) fill.close();
		remote.close();
		requests.close();
		publication.broadcast.close();
		client.abort();
		server.abort();
		origin.close();
		installed.close();
	}
});

// A copy holding group 2 from frame 3 only, as one whose subscription started mid-group does, asks
// its publisher for the head over FETCH once a bounded reader wants the group whole. The publisher
// refused group 2 to the subscription and has it only once the FETCH asks. On lite-07 that refusal's
// SUBSCRIBE_DROP reaches the copy after the head is asked, and leaves the head to its FETCH.
test.each([Lite.ALPN_05, Lite.ALPN_06, Lite.ALPN_07_WIP])(
	"%s: a bounded reader of a copy's headless group gets it whole over FETCH",
	async (alpn) => {
		// The copy the session receives, caught as its subscription starts.
		let copy: Producer | undefined;
		const internal = LiteSubscriber.prototype as unknown as {
			runResponses: (stream: unknown, entry: { track: Producer }) => Promise<void>;
		};
		const runResponses = internal.runResponses;
		const spy = spyOn(internal, "runResponses").mockImplementation(function (
			this: unknown,
			stream: unknown,
			entry: { track: Producer },
		) {
			copy ??= entry.track;
			return runResponses.call(this, stream, entry);
		});
		// The groups the publisher serves over FETCH, in order.
		const fetched: bigint[] = [];
		const runFetch = LitePublisher.prototype.runFetch;
		const fetchSpy = spyOn(LitePublisher.prototype, "runFetch").mockImplementation(function (
			this: LitePublisher,
			msg,
			stream,
		) {
			fetched.push(msg.group);
			return runFetch.call(this, msg, stream);
		});

		const pair = createMockTransportPair(alpn);
		const origin = new OriginProducer();
		const [client, server] = await Promise.all([
			connectSession({ url, transport: pair.client }),
			acceptSession({ transport: pair.server, url, publish: origin.consume() }),
		]);
		const broadcast = origin.route(Path.from("room")).broadcast;
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		write(track.createGroup(3n), 3n, "live");
		// The archive has group 2 once `archived`, and never refuses it as lost.
		let archived = false;
		const requests = track.requests();
		void (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
				for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
					if (key !== 2n || !archived) {
						fill.refuseGroups(key, key + 1n, new StreamError(StreamCode.NotFound));
						continue;
					}
					const group = defined(fill.accept(key));
					for (let frame = 0; frame < 5; frame++)
						group.writeFrame({ payload: enc.encode(`c${frame}`), timestamp: Timestamp.fromMillis(2000) });
					group.finish();
				}
				fill.close();
			}
		})();
		const remote = wireOf(client).consume(Path.from("room"));
		try {
			// Reads from the oldest group, so a bounded reader widens nothing.
			const open = remote.track("video").subscribe(withMax({ delay: { max: null } }));
			expect((await open.recvGroup())?.start).toBe(3n);
			if (!copy) throw new Error("no copy");
			const tail = hooks.receiveGroup(copy, { start: 2n, offset: 3 });
			for (const frame of [3, 4])
				tail.writeFrame({ payload: enc.encode(`c${frame}`), timestamp: Timestamp.fromMillis(2000) });
			tail.finish();
			archived = true;
			const before = fetched.length;

			const bounded = remote.track("video").subscribe(
				withMax({
					ranges: [{ start: 2n, end: 2n }],
					delay: { max: null },
				}),
			);
			const group = await bounded.recvGroup();
			if (!group) throw new Error("no group");
			const got: string[] = [];
			for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
				got.push(dec.decode(frame.payload));
			expect(got).toEqual(["c0", "c1", "c2", "c3", "c4"]);
			expect(fetched.slice(before).filter((group) => group === 2n)).toEqual([2n]);
			bounded.close();
			open.close();
		} finally {
			spy.mockRestore();
			fetchSpy.mockRestore();
			remote.close();
			requests.close();
			broadcast.close();
			client.abort();
			server.abort();
			origin.close();
		}
	},
);

// A publisher whose cache holds group 2 from frame 3 only has its serving reader fetch the head
// from the track's handler and serves the group whole to a subscriber reading whole groups, as the
// Rust `position_group` does: a cache only makes things faster. A refused head drops the group.
// Routed, the publisher serves a relay's front, whose route copy asks that route's handler.
test.each([
	[Lite.ALPN_06, "local"],
	[Lite.ALPN_07_WIP, "local"],
	[Lite.ALPN_07_WIP, "routed"],
] as const)("%s: a %s publisher fetches a headless group's head", async (alpn, via) => {
	for (const refuse of [false, true]) {
		const pair = createMockTransportPair(alpn);
		const origin = new OriginProducer();
		const [client, server] = await Promise.all([
			connectSession({ url, transport: pair.client }),
			acceptSession({ transport: pair.server, url, publish: origin.consume() }),
		]);
		let broadcast: BroadcastProducer;
		let unroute = () => {};
		if (via === "local") {
			broadcast = origin.route(Path.from("room")).broadcast;
		} else {
			// A relay: the broadcast arrives over a route as a logical front, which the publishing
			// origin serves.
			broadcast = new BroadcastProducer();
			const source = broadcast;
			const upstream = new OriginProducer();
			const instance = Instance.parse("01900000-0000-7000-8000-000000000001");
			const received = wireOf(upstream).receive(Path.from("room"), WireRoute.normalize({ instance, cost: 1n }));
			void (async () => {
				for await (const request of received.requested()) request.serve(source.consume());
			})();
			const routed = upstream.consume().request(Path.from("room"));
			const claim = origin.claim(Path.from("room"));
			void (async () => {
				for await (const request of claim.requested()) {
					const front = routed.active.peek();
					if (front) request.serve(front.clone());
					else request.reject(new Error("no route"));
				}
			})();
			unroute = () => {
				claim.close();
				routed.close();
				received.close();
				upstream.close();
			};
		}
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		const tail = hooks.receiveGroup(track, { start: 2n, offset: 3 });
		for (const frame of [3, 4])
			tail.writeFrame({ payload: enc.encode(`c${frame}`), timestamp: Timestamp.fromMillis(2000) });
		tail.finish();
		write(track.createGroup(3n), 3n, "live");
		const requests = track.requests();
		void (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
				for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
					if (key !== 2n || refuse) {
						fill.refuseGroups(key, key + 1n, new StreamError(StreamCode.NotFound));
						continue;
					}
					const group = defined(fill.accept(key));
					for (let frame = 0; frame < 5; frame++)
						group.writeFrame({ payload: enc.encode(`c${frame}`), timestamp: Timestamp.fromMillis(2000) });
					group.finish();
				}
				fill.close();
			}
		})();
		const remote = wireOf(client).consume(Path.from("room"));
		try {
			const reader = remote.track("video").subscribe(
				withMax({
					ranges: [{ start: 2n }],
					delay: { max: null },
					order: "ascending",
				}),
			);
			const ordered = reader.ordered();
			const got: [bigint, string[]][] = [];
			for (let group = await ordered.nextGroup(); group; group = await ordered.nextGroup()) {
				const frames: string[] = [];
				for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
					frames.push(dec.decode(frame.payload));
				got.push([group.start, frames]);
				if (group.start === 3n) break;
			}
			const head: [bigint, string[]][] = refuse ? [] : [[2n, ["c0", "c1", "c2", "c3", "c4"]]];
			expect(got).toEqual([...head, [3n, ["live 3"]]]);
			reader.close();
		} finally {
			remote.close();
			requests.close();
			unroute();
			broadcast.close();
			client.abort();
			server.abort();
			origin.close();
		}
	}
});

// A lite-06 copy of a timestamp-keyed track whose publisher never says which keys between its
// group starts are empty, and resets its first groups as stale after their headers went out: an
// ordered reader reaches the later groups, and the copy asks upstream about the keys between group
// starts once per run, not a FETCH per key. Mirrors the Rust `lite06_fill_walk`. The ordered reader
// alone, from the first group; then behind a live reader holding the copy, joining once the resets
// are over.
test.each([
	[false, 0],
	[true, 4],
] as const)("copy fill: lite-06 asks a run of undeclared keys once (live %p)", async (live, join) => {
	// One group per second of media, keyed by its start in microseconds; the first three reset.
	const GROUP = 1_000_000;
	const FRAME = 33_333;
	const FRAMES = 30;
	const GROUPS = 8;
	const RESET = 3;
	const clock = new Manual();
	const installed = clock.install();
	const pair = createMockTransportPair(Lite.ALPN_06);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("room"));
	const track = defined(
		publication.broadcast.createTrack("video", { timescale: Timescale.MICRO, maxAge: Duration.secs(30) }),
	);
	const remote = wireOf(client).consume(Path.from("room"));
	// Every FETCH and SUBSCRIBE the copy sends opens a bidi stream of its own.
	const requests = () => pair.client.sendStreams.bidi.length;
	const held = live ? remote.track("video").subscribe() : undefined;
	try {
		let read: Promise<bigint[]> | undefined;
		let before = 0;
		for (let index = 0; index < GROUPS; index++) {
			if (index === join) {
				before = requests();
				const reader = remote
					.track("video")
					.subscribe(withMax({ delay: { max: Duration.secs(30) } }))
					.ordered();
				read = (async () => {
					const keys: bigint[] = [];
					for (let group = await reader.nextGroup(); group; group = await reader.nextGroup()) {
						// A group reset as stale ends its read with that error.
						while (await group.readFrame().catch(() => undefined)) {}
						keys.push(group.start);
						if (group.start === BigInt((GROUPS - 1) * GROUP)) break;
					}
					return keys;
				})();
			}
			// Created rather than appended: like a publisher that cannot say, the track never
			// declares the keys between its group starts skipped.
			const start = index * GROUP;
			const group = track.createGroup(BigInt(start));
			for (let frame = 0; frame < FRAMES; frame++) {
				group.writeFrame({
					payload: enc.encode("frame"),
					timestamp: Timestamp.fromMicros(start + frame * FRAME),
				});
				await clock.advance(Duration.micros(FRAME));
			}
			if (index < RESET) group.abort(new StreamError(StreamCode.Old, { message: "old" }));
			else group.finish();
		}
		let done: bigint[] | undefined;
		void read?.then((keys) => {
			done = keys;
		});
		for (let waited = 0; done === undefined && waited < 120; waited++) await clock.advance(Duration.secs(1));
		expect(done?.at(-1)).toBe(BigInt((GROUPS - 1) * GROUP));
		// At most one request per group: never one per key between them.
		expect(requests() - before).toBeLessThanOrEqual(GROUPS);
	} finally {
		held?.close();
		remote.close();
		publication.broadcast.close();
		client.abort();
		server.abort();
		origin.close();
		installed.close();
	}
});
