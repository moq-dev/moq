// A received copy asks its publisher for the groups its readers miss, over lite FETCH, as the Rust
// lite subscriber's `FillServeRun` does: below where the copy's feed started, by the copy's start
// rule, and inside the copy, once it evicted what it received.
import { expect, test } from "bun:test";
import { Elapsed, Manual, Milli, Nano, sleep, timeout } from "@moq/clock";
import { accept as acceptSession, connect as connectSession } from "../connection/index.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { hooks } from "../internal.ts";
import { CACHE_EXPIRY } from "../ledger.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Fill, Producer, Subscriber, Subscription } from "../track.ts";
import { wireOf } from "../wire.ts";
import * as Lite from "./index.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();
const url = new URL("https://localhost:4443/test");

// A JS publisher's track and a JS subscriber's session to it. The publisher's archive serves
// every group its cache no longer holds, except group 1, which it never had.
async function peers(alpn: string) {
	const pair = createMockTransportPair(alpn);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const broadcast = publication.broadcast;
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	const requests = track.requests();
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			for (let sequence = fill.wanted(); sequence !== undefined; sequence = fill.wanted()) {
				if (sequence === 1n) {
					fill.dropGroups(1n, 2n);
					continue;
				}
				write(fill.accept(sequence), sequence, "old");
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
function write(group: ReturnType<Producer["createGroup"]>, sequence: bigint, kind: string): void {
	group.writeFrame({
		payload: enc.encode(`${kind} ${sequence}`),
		timestamp: Timestamp.fromMillis(Number(sequence) * 1000),
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
		got.set(group.sequence, await payload(group));
	}
	return got;
}

// Whether `reader` hands out no group for a while.
async function idle(reader: Subscriber): Promise<boolean> {
	try {
		(await timeout(Nano.fromMilli(Milli(200)), reader.recvGroup()))?.close();
		return false;
	} catch (err) {
		if (err instanceof Elapsed) return true;
		throw err;
	}
}

// Groups 0 to 4, one per second: a group is as old as the start of the one after it, so a
// reader with a half-second budget starts at group 3, on lite-06 and later.
const NARROW: Subscription = { delay: { max: Milli(500) } };
const WIDE: Subscription = { delay: { max: null }, order: "ascending" };

function populate(track: Producer): void {
	for (let sequence = 0n; sequence < 5n; sequence++) write(track.createGroup(sequence), sequence, "live");
}

// What `take` read, in sequence order.
function sorted(got: Map<bigint, string>): [bigint, string][] {
	return [...got].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}

const ALL = [0n, 1n, 2n, 3n, 4n].map((sequence): [bigint, string] => [sequence, `live ${sequence}`]);

test("copy fill: lite-07 fills below a start resolved from the budget, as far as the reader's own budget reaches", async () => {
	const peer = await peers(Lite.ALPN_07_WIP);
	try {
		populate(peer.track);
		const narrow = peer.remote.track("video").subscribe(NARROW);
		expect((await narrow.recvGroup())?.sequence).toBe(3n);
		const before = peer.fetches();

		// The copy cannot tell which budget picked its start, so it fills what this one reaches.
		const wide = peer.remote.track("video").subscribe(WIDE);
		expect(sorted(await take(wide, 5))).toEqual(ALL);
		expect(peer.fetches()).toBeGreaterThan(before);
		narrow.close();
		wide.close();
	} finally {
		peer.close();
	}
});

test("copy fill: lite-06 asks nothing below a start resolved from the budget, and relies on the re-offer", async () => {
	const peer = await peers(Lite.ALPN_06);
	try {
		populate(peer.track);
		const narrow = peer.remote.track("video").subscribe(NARROW);
		expect((await narrow.recvGroup())?.sequence).toBe(3n);
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
	const peer = await peers(Lite.ALPN_05);
	try {
		populate(peer.track);
		const live = peer.remote.track("video").subscribe(NARROW);
		expect((await live.recvGroup())?.sequence).toBe(4n);
		const before = peer.fetches();

		// Reaching back to the oldest group names no floor, so the live edge bounds it.
		const oldest = peer.remote.track("video").subscribe(WIDE);
		expect((await oldest.recvGroup())?.sequence).toBe(4n);
		expect(await idle(oldest)).toBe(true);
		expect(peer.fetches()).toBe(before);

		// A floor the ranges name is filled.
		const named = peer.remote.track("video").subscribe({ ...WIDE, ranges: [{ start: { absolute: 0n } }] });
		expect(sorted(await take(named, 5))).toEqual(ALL);
		expect(peer.fetches()).toBeGreaterThan(before);
		live.close();
		oldest.close();
		named.close();
	} finally {
		peer.close();
	}
});

test.each([Lite.ALPN_05, Lite.ALPN_06, Lite.ALPN_07_WIP])(
	"copy fill: %s fills what the copy evicted from the publisher's archive",
	async (alpn) => {
		const clock = new Manual();
		const installed = clock.install();
		const peer = await peers(alpn);
		try {
			const history: Subscription = {
				ranges: [{ start: { absolute: 0n } }],
				delay: { max: null },
				order: "ascending",
			};
			const first = peer.remote.track("video").subscribe(history);
			for (let sequence = 0n; sequence < 4n; sequence++)
				write(peer.track.createGroup(sequence), sequence, "live");
			// Before lite-07 a subscription carries no order and is served newest first, so only
			// the set of groups is fixed.
			expect(sorted(await take(first, 4)).map(([sequence]) => sequence)).toEqual([0n, 1n, 2n, 3n]);

			// Both caches age out what nobody touched, a prune slice past the window; only the
			// live edge stays.
			await clock.advance(Nano.fromMilli(Milli(CACHE_EXPIRY + 10_000)));
			write(peer.track.createGroup(4n), 4n, "live");
			expect((await first.recvGroup())?.sequence).toBe(4n);
			const before = peer.fetches();

			// The same ranges, so nothing changes upstream: only a FETCH brings what the copy lost.
			const again = peer.remote.track("video").subscribe(history);
			const got = await take(again, 4);
			expect(sorted(got)).toEqual([
				[0n, "old 0"],
				[2n, "old 2"],
				[3n, "old 3"],
				[4n, "live 4"],
			]);
			expect(peer.fetches()).toBe(before + 4);
			first.close();
			again.close();
		} finally {
			peer.close();
			installed.close();
		}
	},
);

test("copy fill: a filled group nobody holds any more stops downloading", async () => {
	const pair = createMockTransportPair(Lite.ALPN_07_WIP);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const track = publication.broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	write(track.createGroup(1n), 1n, "live");
	// The archive answers group 0 with one frame and never finishes it.
	const requests = track.requests();
	const fills: Fill[] = [];
	const archived = (async () => {
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill");
		fills.push(fill);
		const group = fill.accept(0n);
		group.writeFrame({ payload: enc.encode("old 0"), timestamp: Timestamp.fromMillis(0) });
		return group;
	})();
	const remote = wireOf(client).consume(Path.from("room"));
	try {
		// A live reader keeps the copy up. It reads in order, so it lets go of what it passes.
		const reader = remote.track("video").subscribe({ ranges: [{ start: { relative: 0n } }] });
		const live = reader.ordered();
		expect((await live.nextGroup())?.sequence).toBe(1n);
		const history = remote
			.track("video")
			.subscribe({ ...WIDE, ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }] });
		const group = await history.recvGroup();
		expect(group?.sequence).toBe(0n);
		expect(dec.decode((await group?.readFrame())?.payload)).toBe("old 0");
		const source = await archived;
		expect(source.demand().wanted.peek()).toBeTruthy();

		// The history reader leaves mid-group, and the live one reads past it: nobody holds the
		// group any more, so the copy cancels its FETCH and aborts the truncated group rather
		// than keep downloading it.
		group?.close();
		history.close();
		write(track.createGroup(2n), 2n, "live");
		expect((await live.nextGroup())?.sequence).toBe(2n);
		await timeout(
			Nano.fromMilli(Milli(2000)),
			(async () => {
				while (hooks.holdsGroup(reader, 0n)) await sleep(Nano.fromMilli(Milli(10)));
			})(),
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
	}
});
