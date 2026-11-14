// How each reader's disposition record settles drops, skips, and evictions through a relay: a
// publisher's session feeds a relay's received copy, which serves a viewer's session from it,
// all on mock transports and a manual clock. Mirrors rs/moq-net/tests/integration/drop_record.rs.

import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { Duration, Elapsed, type Manual, timeout } from "@moq/clock";
import * as cache from "../cache.ts";
import { accept, connect } from "../connection/index.ts";
import { StreamCode, StreamError, TooFarBehind, toStreamCode } from "../error.ts";
import { hooks, type Recv, staleHole } from "../internal.ts";
import { createMockTransportPair, manual } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import type { Reader } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Subscription, Producer as TrackProducer, Subscriber as TrackSubscriber } from "../track.ts";
import { wireOf } from "../wire.ts";
import { SubscribeDrop, SubscribeSkip, SubscribeStart, SubscribeUpdate } from "./subscribe.ts";
import { ALPN_06, ALPN_07_WIP, Version } from "./version.ts";

const url = new URL("https://localhost:4443/test");
const path = Path.from("room");

afterEach(() => mock.restore());

// A hop of the chain: the publisher to the relay, or the relay to the viewer.
type Hop = "up" | "down";

// A SUBSCRIBE_DROP or SUBSCRIBE_SKIP a subscriber decoded, and on which hop.
interface Heard {
	hop: Hop;
	kind: "drop" | "skip";
	start: bigint;
	end: bigint;
	error?: number;
	update?: bigint;
}

/**
 * Listen to every subscribe response the chain decodes. A hop is named by the order its first
 * SUBSCRIBE_START arrives: the relay's upstream START always comes first, since its downstream
 * START waits on it. A drop matching `hold` is kept from its subscriber until released, as one
 * still in flight.
 */
function tap() {
	const hops = new Map<Reader, Hop>();
	const heard: Heard[] = [];
	const updates: SubscribeUpdate[] = [];
	const held: (() => void)[] = [];
	let hold: ((heard: Heard) => boolean) | undefined;

	const start = SubscribeStart.decode.bind(SubscribeStart);
	spyOn(SubscribeStart, "decode").mockImplementation(async (r, version) => {
		if (!hops.has(r)) hops.set(r, hops.size === 0 ? "up" : "down");
		return start(r, version);
	});
	const drop = SubscribeDrop.decode.bind(SubscribeDrop);
	spyOn(SubscribeDrop, "decode").mockImplementation(async (r, version) => {
		const msg = await drop(r, version);
		const entry: Heard = {
			hop: hops.get(r) ?? "down",
			kind: "drop",
			start: msg.start,
			end: msg.end,
			error: msg.error,
			update: msg.update,
		};
		heard.push(entry);
		if (hold?.(entry)) await new Promise<void>((release) => held.push(release));
		return msg;
	});
	const skip = SubscribeSkip.decode.bind(SubscribeSkip);
	spyOn(SubscribeSkip, "decode").mockImplementation(async (r, version) => {
		const msg = await skip(r, version);
		heard.push({ hop: hops.get(r) ?? "down", kind: "skip", start: msg.start, end: msg.end });
		return msg;
	});
	const update = SubscribeUpdate.decodeMaybe.bind(SubscribeUpdate);
	spyOn(SubscribeUpdate, "decodeMaybe").mockImplementation(async (r, version) => {
		const msg = await update(r, version);
		if (msg) updates.push(msg);
		return msg;
	});

	return {
		heard,
		updates,
		held,
		/** The drops and skips heard on `hop` from entry `from` on, as `[kind, start, end, code, update]`. */
		on(hop: Hop, from = 0) {
			return heard
				.slice(from)
				.filter((entry) => entry.hop === hop)
				.map((entry) => [entry.kind, entry.start, entry.end, entry.error, entry.update]);
		},
		hold(fn: ((heard: Heard) => boolean) | undefined) {
			hold = fn;
		},
		/** Hand every held drop to its subscriber. */
		release() {
			for (const release of held.splice(0)) release();
		},
	};
}

/** Every track's cache pool, in the order the tracks open. */
function pools(): { producer: TrackProducer; pool: cache.Pool }[] {
	const opened: { producer: TrackProducer; pool: cache.Pool }[] = [];
	const original = hooks.poolTrack;
	spyOn(hooks, "poolTrack").mockImplementation((producer, pool) => {
		opened.push({ producer, pool });
		original(producer, pool);
	});
	return opened;
}

/**
 * A publisher's session on `up` to a relay, and the relay's session on `down` to a viewer. The
 * relay serves the viewer from its received copy of the publisher's broadcast, as a JS relay does.
 * The publisher's track caches into a pool that never ages a group out, so it still holds what
 * the relay and the viewer evict on the manual clock, unless a test resizes it.
 */
async function chain(up: Version, down: Version) {
	const alpn = (version: Version) => (version === Version.DRAFT_07 ? ALPN_07_WIP : ALPN_06);
	const upstream = createMockTransportPair(alpn(up));
	const downstream = createMockTransportPair(alpn(down));
	const source = new OriginProducer();
	const relay = new OriginProducer();
	const [publisher, ingest, serve, viewer] = await Promise.all([
		connect({ url, transport: upstream.client, publish: source.consume() }),
		accept({ transport: upstream.server, url, version: up }),
		accept({ transport: downstream.server, url, version: down, publish: relay.consume() }),
		connect({ url, transport: downstream.client }),
	]);
	const sessions = [publisher, ingest, serve, viewer];
	let ended = false;
	for (const session of sessions) {
		void session.closed.then(() => {
			ended = true;
		});
	}

	const claim = relay.claim(path);
	const serving = (async () => {
		for await (const request of claim.requested()) request.serve(wireOf(ingest).consume(request.path));
	})();
	const broadcast = source.route(path).broadcast;
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	const pool = new cache.Pool();
	hooks.poolTrack(track, pool);
	const remote = wireOf(viewer).consume(path);

	return {
		track,
		pool,
		remote,
		/** Write group `sequence` presenting at `millis`, with `size` bytes of payload. */
		write(sequence: bigint, millis: number, size = 1) {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: new Uint8Array(size), timestamp: Timestamp.fromMillis(millis) });
			group.finish();
		},
		/** How many FETCHes the relay sent upstream, and the viewer sent the relay. */
		fetches() {
			return { up: upstream.server.sendStreams.bidi.length, down: downstream.client.sendStreams.bidi.length };
		},
		/** Whether any session closed. */
		ended: () => ended,
		/** Wait until the publisher's track has a subscriber through the relay. */
		async reached() {
			const demand = track.demand();
			while (demand.wanted.peek() === undefined) await demand.wanted.changed();
		},
		async close() {
			remote.close();
			broadcast.close();
			claim.close();
			await serving;
			for (const session of sessions) session.abort();
			source.close();
			relay.close();
		},
	};
}

/**
 * The next thing `reader` settles: a group, a drop or skip, its end, or its error. Once `signal`
 * aborts it stops reading and never resolves, so a read given up takes nothing from the reader.
 */
async function next(reader: TrackSubscriber, signal?: AbortSignal): Promise<Recv> {
	for (;;) {
		if (signal?.aborted) return new Promise<Recv>(() => {});
		const recv = hooks.tryRecvGroup(reader, true);
		if (recv.kind !== "idle" && recv.kind !== "boundary") return recv;
		await new Promise<void>((resolve) => {
			const wake = () => {
				dispose();
				signal?.removeEventListener("abort", wake);
				resolve();
			};
			const dispose = hooks.groupChanged(reader, wake);
			signal?.addEventListener("abort", wake);
		});
	}
}

/** The next thing `reader` settles, or undefined once `ms` of manual time passed without it. */
async function within(clock: Manual, reader: TrackSubscriber, ms = 1000): Promise<Recv | undefined> {
	const controller = new AbortController();
	try {
		return await clock.drive(timeout(Duration.millis(ms), next(reader, controller.signal)));
	} catch (err) {
		if (err instanceof Elapsed) return undefined;
		throw err;
	} finally {
		// A read still pending would otherwise take the next group from whoever reads after.
		controller.abort();
	}
}

/** Drain pending work, then step manual time 10 ms at a time, until `ready` holds. */
async function until(clock: Manual, ready: () => boolean): Promise<void> {
	await clock.advance(Duration.ZERO);
	for (let i = 0; i < 1000 && !ready(); i++) await clock.advance(Duration.millis(10));
	expect(ready()).toBe(true);
}

/** What a read settled, in a comparable shape. */
function settled(recv: Recv | undefined): unknown[] {
	if (!recv) return ["nothing"];
	switch (recv.kind) {
		case "group":
			return ["group", recv.group.sequence];
		case "drop":
			return [recv.skip ? "skip" : "drop", recv.start, recv.end, toStreamCode(recv.error)];
		case "error":
			return ["error", recv.error];
		default:
			return [recv.kind];
	}
}

/** Everything `reader` settles until it goes quiet for a second of manual time, or ends. */
async function drain(clock: Manual, reader: TrackSubscriber): Promise<unknown[][]> {
	const got: unknown[][] = [];
	for (;;) {
		const recv = await within(clock, reader);
		if (!recv) return got;
		got.push(settled(recv));
		if (recv.kind === "done" || recv.kind === "error") return got;
	}
}

/** The sequences of the groups among `rows`, in order. */
function groupsOf(rows: unknown[][]): unknown[] {
	return rows
		.filter((row) => row[0] === "group")
		.map((row) => row[1] as bigint)
		.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}

const WIDE: Subscription = { delay: { max: null }, order: "ascending", ranges: [{ start: { absolute: 0n } }] };

// Groups 1 to 4 after group 0 at 0s: 1 and 2 end 3s and 4s behind the 6s edge, and 3 starts 3s
// behind it, so a 1.5s budget gives up 1 and 2 and still takes 3.
function behindTheEdge(write: (sequence: bigint, millis: number) => void) {
	for (const [sequence, millis] of [
		[1n, 1000],
		[2n, 2000],
		[3n, 3000],
		[4n, 6000],
	] as const)
		write(sequence, millis);
}

// A reader with a 1.5s budget is reported groups 1 and 2 stale through the relay, and those OLD
// drops are still in flight to the viewer when it widens to 10s, which the publisher, having
// evicted both, can only drop again. With `prompt`, that re-drop goes out under the widening
// before the track ends; otherwise the publisher's end sweep names it.
async function widening(prompt: boolean): Promise<void> {
	using clock = manual();
	const wire = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
	const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(1500) } });
	await chained.reached();
	chained.write(0n, 0);
	expect(settled(await next(reader))).toEqual(["group", 0n]);

	wire.hold((heard) => heard.hop === "down" && heard.update === 0n);
	behindTheEdge(chained.write);
	expect([settled(await next(reader)), settled(await next(reader))]).toEqual([
		["group", 3n],
		["group", 4n],
	]);
	await until(clock, () => wire.held.length > 0);
	expect(wire.on("up")).toEqual([
		["drop", 1n, 2n, StreamCode.Old, 0n],
		["drop", 2n, 3n, StreamCode.Old, 0n],
	]);

	// The publisher no longer holds them: it evicts everything but the live edge.
	chained.pool.resize(1);
	chained.write(5n, 6100);
	expect(settled(await next(reader))).toEqual(["group", 5n]);

	// The widening reaches both hops before the old drops land, which then settle nothing.
	reader.update({ delay: { max: Duration.millis(10_000) } });
	await until(clock, () => wire.updates.length >= 2);
	expect(wire.updates.map((update) => update.update)).toEqual([1n, 1n]);
	wire.hold((heard) => heard.hop === "down" && heard.update === 1n);
	wire.release();
	await until(clock, () => wire.on("down").length >= 2);
	expect(hooks.staleHeld(reader, 1n, 3n)).toEqual([]);
	expect(hooks.tryRecvGroup(reader, true).kind).toBe("idle");

	// The re-drop under the widening's own number settles the widened reader for good.
	if (!prompt) chained.track.finish();
	await until(clock, () => wire.held.length > 0);
	expect(wire.on("down")).toEqual([
		["drop", 1n, 2n, StreamCode.Old, 0n],
		["drop", 2n, 3n, StreamCode.Old, 0n],
		["drop", 1n, 3n, StreamCode.NotFound, 1n],
	]);
	expect(hooks.tryRecvGroup(reader, true).kind).toBe("idle");
	wire.hold(undefined);
	wire.release();
	expect(settled(await next(reader))).toEqual(["drop", 1n, 3n, StreamCode.NotFound]);
	if (prompt) chained.track.finish();
	expect(await drain(clock, reader)).toEqual([["done"]]);
	expect(chained.ended()).toBe(false);

	reader.close();
	await chained.close();
}

// Mirrors Rust `after_a_widening_the_old_updates_drop_settles_no_reader` in
// rs/moq-net/tests/integration/drop_record.rs.
test("a drop under the narrower update settles no reader once the widening is sent, and the end's re-drop under it does", () =>
	widening(false));

// Mirrors Rust `after_a_widening_the_old_updates_drop_settles_no_reader` in
// rs/moq-net/tests/integration/drop_record.rs.
test("a widening drops again at once what the publisher evicted", () => widening(true));

// Readers on the viewer while the publisher still holds groups 0 to 5 and the relay and the
// viewer evicted all but group 5: `owed` subscribed before the eviction and read nothing, `fresh`
// subscribes after it, and `widened` gave groups 1 and 2 up as stale and widens after it. Each
// is filled through the relay and never told a group was dropped.
async function relayEvicted(readers: ("owed" | "fresh" | "widened")[]): Promise<void> {
	using clock = manual();
	const wire = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
	// The first reader takes every group, so the relay's copy received them all.
	const first = chained.remote.track("video").subscribe(WIDE);
	const owed = chained.remote.track("video").subscribe(WIDE);
	await chained.reached();
	for (let sequence = 0n; sequence < 5n; sequence++) chained.write(sequence, Number(sequence) * 1000);
	const read: unknown[][] = [];
	for (let i = 0; i < 5; i++) read.push(settled(await next(first)));
	expect(groupsOf(read)).toEqual([0n, 1n, 2n, 3n, 4n]);
	// Half a second gives up groups 1 and 2 as stale, on the viewer alone. Group 0 is the
	// reader's start or stale too, depending on which group reached it first.
	const widened = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(500) } });
	const stale = await drain(clock, widened);
	const given = stale.filter((row) => row[0] === "drop");
	expect(given.every((row) => row[3] === StreamCode.Old && row[2] === (row[1] as bigint) + 1n)).toBe(true);
	const givenUp = given.map((row) => row[1] as bigint);
	expect(givenUp).toEqual(expect.arrayContaining([1n, 2n]));
	const accounted = [...givenUp, ...(groupsOf(stale) as bigint[])];
	expect(accounted.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))).toEqual([0n, 1n, 2n, 3n, 4n]);
	expect(groupsOf(stale)).toEqual(expect.arrayContaining([3n, 4n]));

	// The relay and the viewer age out everything but the live edge; the publisher keeps it all.
	await clock.advance(cache.DEFAULT_EXPIRY.add(Duration.millis(10_000)));
	chained.write(5n, 5000);
	expect(settled(await next(first))).toEqual(["group", 5n]);
	const before = chained.fetches();

	const all = [0n, 1n, 2n, 3n, 4n, 5n];
	for (const name of readers) {
		let reader: TrackSubscriber;
		let expected = all;
		if (name === "owed") reader = owed;
		else if (name === "fresh") reader = chained.remote.track("video").subscribe(WIDE);
		else {
			reader = widened;
			reader.update(WIDE);
			expected = [...givenUp, 5n];
		}
		const rows = await drain(clock, reader);
		expect(rows.filter((row) => row[0] !== "group")).toEqual([]);
		expect(groupsOf(rows)).toEqual(expected);
		reader.close();
	}
	// Filled through the relay from the publisher, and never named dropped on either hop.
	const after = chained.fetches();
	expect(after.up).toBeGreaterThan(before.up);
	expect(after.down).toBeGreaterThan(before.down);
	expect(wire.heard).toEqual([]);
	expect(chained.ended()).toBe(false);

	for (const reader of [first, owed, widened]) reader.close();
	await chained.close();
}

// Mirrors Rust `a_group_the_relay_evicted_is_filled_never_dropped` in
// rs/moq-net/tests/integration/drop_record.rs.
test("a group the relay evicted while the publisher holds it is filled for a new and an owed reader", () =>
	relayEvicted(["fresh", "owed"]));

// Mirrors Rust `a_group_the_relay_evicted_is_filled_never_dropped` in
// rs/moq-net/tests/integration/drop_record.rs.
test("a group the relay evicted while the publisher holds it is filled for a widened reader", () =>
	relayEvicted(["widened"]));

// Mirrors Rust `an_evicted_drop_is_forwarded_unchanged_and_never_reoffered` in
// rs/moq-net/tests/integration/drop_record.rs.
test("an origin's evicted drops cross the relay unchanged, and no update re-offers them", async () => {
	using clock = manual();
	const wire = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
	const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(2000) } });
	await chained.reached();
	const evicted = () => ({ error: new StreamError(StreamCode.Evicted, { message: "group evicted from the cache" }) });
	chained.write(0n, 0);
	expect(settled(await next(reader))).toEqual(["group", 0n]);
	// Group 1 the origin's cache evicted, and group 3 its retention expired: both EVICTED, never OLD.
	hooks.dropTrackGroups(chained.track, 1n, 2n, evicted());
	chained.write(2n, 200);
	hooks.dropTrackGroups(chained.track, 3n, 4n, evicted());
	chained.write(4n, 400);
	const got: unknown[][] = [];
	for (let i = 0; i < 4; i++) got.push(settled(await next(reader)));
	expect(groupsOf(got)).toEqual([2n, 4n]);
	expect(got.filter((row) => row[0] !== "group")).toEqual([
		["drop", 1n, 2n, StreamCode.Evicted],
		["drop", 3n, 4n, StreamCode.Evicted],
	]);
	for (const hop of ["up", "down"] as const) {
		expect(wire.on(hop)).toEqual([
			["drop", 1n, 2n, StreamCode.Evicted, 0n],
			["drop", 3n, 4n, StreamCode.Evicted, 0n],
		]);
	}

	// A widening and a narrowing each reach the publisher, and neither drops nor offers them again.
	const heard = wire.heard.length;
	reader.update({ delay: { max: Duration.millis(60_000) } });
	await until(clock, () => wire.updates.length >= 2);
	reader.update({ delay: { max: Duration.millis(1000) } });
	await until(clock, () => wire.updates.length >= 4);
	chained.track.finish();
	expect(await drain(clock, reader)).toEqual([["done"]]);
	expect(wire.heard.slice(heard)).toEqual([]);
	expect(chained.ended()).toBe(false);

	reader.close();
	await chained.close();
});

// Mirrors Rust `a_lite06_hop_keeps_a_reported_group_dropped` in
// rs/moq-net/tests/integration/drop_record.rs.
for (const down of [Version.DRAFT_06, Version.DRAFT_07]) {
	const name = down === Version.DRAFT_07 ? "lite-07" : "lite-06";
	test(`through a lite-06 hop, a reported group stays dropped for every ${name} reader, and a shrink sends no drops`, async () => {
		using clock = manual();
		const wire = tap();
		const chained = await chain(Version.DRAFT_06, down);
		const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(1500) } });
		await chained.reached();
		chained.write(0n, 0);
		expect(settled(await next(reader))).toEqual(["group", 0n]);
		behindTheEdge(chained.write);
		// Lite-06 names no code, so every drop it carries reads as OLD, and is final.
		const got: unknown[][] = [];
		for (let i = 0; i < 4; i++) got.push(settled(await next(reader)));
		expect(got.filter((row) => row[0] === "drop")).toEqual([
			["drop", 1n, 2n, StreamCode.Old],
			["drop", 2n, 3n, StreamCode.Old],
		]);
		expect(wire.on("up")).toEqual([
			["drop", 1n, 2n, 0, 0n],
			["drop", 2n, 3n, 0, 0n],
		]);

		// A widened reader and a new one each find groups 1 and 2 still dropped, though the
		// publisher holds them.
		reader.update({ delay: { max: null } });
		const fresh = chained.remote.track("video").subscribe(WIDE);
		expect(groupsOf(await drain(clock, fresh))).toEqual([0n, 3n, 4n]);
		expect(groupsOf(await drain(clock, reader))).toEqual([]);

		// A shrink sends no drops upstream, nor any new one downstream.
		fresh.close();
		const heard = wire.heard.length;
		const updates = wire.updates.length;
		reader.update({ delay: { max: Duration.millis(500) } });
		await until(clock, () => wire.updates.length >= updates + 2);
		await drain(clock, reader);
		expect(wire.on("up", heard)).toEqual([]);
		for (const row of wire.on("down", heard)) expect([row[1], row[2]]).toEqual([1n, 3n]);
		chained.track.finish();
		expect(await drain(clock, reader)).toEqual([["done"]]);
		expect(chained.ended()).toBe(false);

		reader.close();
		await chained.close();
	});
}

// Mirrors Rust `a_lite06_upstream_drop_is_fetched_for_a_widened_lite07_viewer` in
// rs/moq-net/tests/integration/drop_record.rs.
test("through a lite-06 upstream, a group reported dropped as it lands is fetched for a widened lite-07 viewer", async () => {
	using clock = manual();
	const chained = await chain(Version.DRAFT_06, Version.DRAFT_07);
	for (let sequence = 0n; sequence < 10n; sequence++) {
		if (sequence !== 7n) chained.write(sequence, Number(sequence) * 1000);
	}
	// Reaches groups 5 and up by media time.
	const first = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(3500) }, ranges: [{}] });
	await chained.reached();
	expect(groupsOf(await drain(clock, first))).toEqual([5n, 6n, 8n, 9n]);

	// Group 7 lands once its successor is older than 3.5s by the clock, so the lite-06
	// publisher reports it dropped, final for the relay's subscription but judged under its
	// budget: the publisher still holds the group.
	await clock.advance(Duration.millis(4500));
	chained.write(7n, 7000);
	expect(groupsOf(await drain(clock, first))).toEqual([]);

	// Reaches every group: the relay forwards the drop to the lite-07 viewer as a stale one, so
	// the widening reopens it, and the widened reader fills 0 to 4 and 7 from the publisher, as
	// a later reader does.
	const all = [0n, 1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n, 9n];
	first.update({ delay: { max: Duration.millis(60_000) }, ranges: [{}] });
	expect(groupsOf(await drain(clock, first))).toEqual([0n, 1n, 2n, 3n, 4n, 7n]);
	const second = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(60_000) }, ranges: [{}] });
	expect(groupsOf(await drain(clock, second))).toEqual(all);

	chained.track.finish();
	for (const reader of [first, second]) expect(groupsOf(await drain(clock, reader))).toEqual([]);
	expect(chained.ended()).toBe(false);

	first.close();
	second.close();
	await chained.close();
});

// Mirrors Rust `pool_pressure_folds_final_runs_and_ends_a_stale_reader` in
// rs/moq-net/tests/integration/drop_record.rs.
for (const stale of [false, true]) {
	const outcome = stale
		? "holding stale reports within reach ends with TooFarBehind"
		: "folds what it settled for good";
	test(`under pool pressure on the relay, a reader ${outcome}`, async () => {
		using clock = manual();
		const opened = pools();
		const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
		const budget = 1000;
		const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(budget) } });
		// The viewer's copy opens as the reader subscribes, then the relay's as the request reaches it.
		await chained.reached();
		const relay = opened[3];
		expect(opened.slice(0, 2).every((entry) => entry.producer === chained.track)).toBe(true);
		expect(relay.producer).not.toBe(chained.track);
		chained.write(0n, 0);
		expect(settled(await next(reader))).toEqual(["group", 0n]);

		// A flood of drops alternating with skips, which the relay forwards and its reader records.
		const runs = 200n;
		const hole = () => (stale ? staleHole(budget) : { error: new StreamError(StreamCode.Cancel) });
		for (let run = 0n; run < runs; run++) {
			hooks.dropTrackGroups(chained.track, 2n * run + 1n, 2n * run + 2n, hole());
			chained.track.skipGroups(2n * run + 2n, 2n * run + 3n);
		}
		let reported = 0n;
		while (reported < 2n * runs) {
			const recv = await next(reader);
			expect(recv.kind).toBe("drop");
			if (recv.kind === "drop") reported += recv.end - recv.start;
		}
		expect(relay.pool.used).toBeGreaterThanOrEqual(Number(2n * runs) * cache.READER_RUN_OVERHEAD);

		// The relay's pool shrinks, and the groups its reader serves accrue the debt: its track
		// sheds what it can first, then the reader pays.
		const capacity = 64 * cache.RUN_OVERHEAD;
		relay.pool.resize(capacity);
		let error: unknown;
		let groups = 0;
		for (let sequence = 2n * runs + 1n; sequence < 2n * runs + 65n && error === undefined; sequence++) {
			chained.write(sequence, budget + Number(sequence), 16 * 1024);
			const recv = await within(clock, reader);
			if (recv?.kind === "error") error = recv.error;
			else {
				expect(settled(recv)).toEqual(["group", sequence]);
				groups++;
			}
		}
		if (stale) {
			expect(error).toBeInstanceOf(TooFarBehind);
		} else {
			expect(error).toBeUndefined();
			expect(groups).toBe(64);
			expect(relay.pool.used).toBeLessThanOrEqual(capacity + 2 * (16 * 1024 + cache.ENTRY_OVERHEAD));
		}
		expect(chained.ended()).toBe(false);

		reader.close();
		await chained.close();
	});
}

// Groups 1 and 2 are judged stale for the relay's subscription, under a 1.5s budget, a run the
// relay's copy shares with all its readers, then the relay `lost` that run: it aged out past the
// pool's idle window, or the pool shed it under pressure. The readers in `readers` subscribe next:
// a narrow one is judged stale for groups 1 and 2 while the viewer still holds the run, and takes
// them once every hop forgot it, and a wide one is filled them through the relay from
// the publisher, which still holds them: fetched once the viewer's copy forgot the run too, and
// re-offered over both subscriptions by the widening update while it still holds it stale.
async function lostRun(lost: "forgot" | "shed", readers: ("wide" | "narrow")[]): Promise<void> {
	using clock = manual();
	const opened = pools();
	const wire = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
	const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(1500) } });
	await chained.reached();
	const relay = opened[3];
	chained.write(0n, 0);
	expect(settled(await next(reader))).toEqual(["group", 0n]);
	behindTheEdge(chained.write);
	const got: unknown[][] = [];
	for (let i = 0; i < 4; i++) got.push(settled(await next(reader)));
	expect(got.filter((row) => row[0] === "drop")).toEqual([
		["drop", 1n, 2n, StreamCode.Old],
		["drop", 2n, 3n, StreamCode.Old],
	]);

	// Past the pool's idle window the relay's copy and the viewer's forget the run; a pool too
	// small for one large group sheds the relay's. The pool then grows back, so the relay holds
	// the groups it is filled next until its readers take them: one too small to hold any would
	// see a refill evicted again, which its reader gives up and names dropped.
	if (lost === "forgot") await clock.advance(cache.DEFAULT_EXPIRY.mul(2));
	else relay.pool.resize(cache.RUN_OVERHEAD);
	chained.write(5n, 6100, 64 * 1024);
	expect(settled(await next(reader))).toEqual(["group", 5n]);
	if (lost === "shed") relay.pool.resize();
	const before = chained.fetches();
	const heard = wire.heard.length;
	let received: bigint[] = [];

	for (const name of readers) {
		if (name === "wide") {
			const wide = chained.remote.track("video").subscribe(WIDE);
			const filled = await drain(clock, wide);
			expect(filled.filter((row) => row[0] !== "group")).toEqual([]);
			received = groupsOf(filled) as bigint[];
			expect(received).toEqual(expect.arrayContaining([1n, 2n]));
			if (lost === "forgot") expect(chained.fetches().up).toBeGreaterThan(before.up);
			wide.close();
		} else {
			// Judged stale by its own budget while the viewer still holds the run. Once every hop forgot
			// it, the relay recovers it with no max delay, as the Rust `Recovery` does, so the narrow
			// reader takes the stale history: no hop still holds a newer group to judge it by.
			const narrow = chained.remote.track("video").subscribe({
				delay: { max: Duration.millis(1500) },
				ranges: [{ start: { absolute: 1n } }],
			});
			const judged = groupsOf(await drain(clock, narrow));
			if (lost === "shed") {
				expect(judged).not.toContain(1n);
				expect(judged).not.toContain(2n);
			} else expect(judged).toEqual([1n, 2n, 3n, 4n, 5n]);
			narrow.close();
		}
	}
	// Nothing a narrow budget gave up is named dropped for good on the wire, where it would settle
	// every reader, wide ones too: a stale drop is reopened by the next update. The update asking
	// for a recovered run may re-offer a group its recovery already delivered, whose duplicate
	// stream the relay resets, which settles no reader.
	if (readers.includes("wide")) {
		const final = wire.heard
			.slice(heard)
			.filter((row) => row.kind !== "drop" || row.error !== StreamCode.Old)
			.filter(
				(row) =>
					!(
						row.kind === "drop" &&
						row.error !== StreamCode.NotFound &&
						row.end === row.start + 1n &&
						received.includes(row.start)
					),
			);
		expect(final).toEqual([]);
	}
	expect(chained.ended()).toBe(false);

	reader.close();
	await chained.close();
}

for (const lost of ["forgot", "shed"] as const) {
	// Mirrors Rust `a_shed_run_is_judged_or_filled_through_the_relay` in
	// rs/moq-net/tests/integration/drop_record.rs.
	test(`a sequence whose run the relay ${lost} is judged by a narrow reader`, () => lostRun(lost, ["narrow"]));

	// Mirrors Rust `a_shed_run_is_judged_or_filled_through_the_relay` in
	// rs/moq-net/tests/integration/drop_record.rs.
	test(`a sequence whose run the relay ${lost} is filled for a wide reader`, () => lostRun(lost, ["narrow", "wide"]));
}

const BILLIONS = 1n << 40n;

// Mirrors Rust `a_run_of_billions_settles_in_one_step` in
// rs/moq-net/tests/integration/drop_record.rs.
test("a run of billions of sequences is settled by each reader in one step, dropped or skipped", async () => {
	using clock = manual();
	const wire = tap();
	const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
	const narrow = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(1500) } });
	const wide = chained.remote.track("video").subscribe({ delay: { max: null }, order: "ascending" });
	await chained.reached();
	chained.write(0n, 0);
	for (const reader of [narrow, wide]) expect(settled(await next(reader))).toEqual(["group", 0n]);
	const before = chained.fetches();

	hooks.dropTrackGroups(chained.track, 1n, 1n + BILLIONS, { error: new StreamError(StreamCode.Cancel) });
	chained.track.skipGroups(1n + BILLIONS, 1n + 2n * BILLIONS);
	chained.write(1n + 2n * BILLIONS, 1000);
	for (const reader of [narrow, wide]) {
		const got: unknown[][] = [];
		for (let i = 0; i < 3; i++) got.push(settled(await within(clock, reader)));
		expect(got).toEqual([
			["drop", 1n, 1n + BILLIONS, StreamCode.Cancel],
			["skip", 1n + BILLIONS, 1n + 2n * BILLIONS, StreamCode.NotFound],
			["group", 1n + 2n * BILLIONS],
		]);
	}
	for (const hop of ["up", "down"] as const) {
		expect(wire.on(hop)).toEqual([
			["drop", 1n, 1n + BILLIONS, StreamCode.Cancel, 0n],
			["skip", 1n + BILLIONS, 1n + 2n * BILLIONS, undefined, undefined],
		]);
	}
	expect(chained.fetches()).toEqual(before);

	for (const reader of [narrow, wide]) reader.close();
	await chained.close();
});

// Mirrors Rust `a_run_of_billions_settles_in_one_step` in
// rs/moq-net/tests/integration/drop_record.rs.
for (const judged of ["reopened by a widening", "forgotten by the relay"] as const) {
	test(`a run of billions ${judged} is judged again in one step`, async () => {
		using clock = manual();
		const chained = await chain(Version.DRAFT_07, Version.DRAFT_07);
		const reader = chained.remote.track("video").subscribe({ delay: { max: Duration.millis(1500) } });
		// An unbounded reader keeps the relay's subscription unbounded too.
		const wide =
			judged === "forgotten by the relay"
				? chained.remote.track("video").subscribe({ delay: { max: null } })
				: undefined;
		await chained.reached();
		chained.write(0n, 0);
		expect(settled(await next(reader))).toEqual(["group", 0n]);
		if (judged === "reopened by a widening")
			hooks.dropTrackGroups(chained.track, 1n, 1n + BILLIONS, staleHole(1500));
		else chained.track.skipGroups(1n, 1n + BILLIONS);
		chained.write(1n + BILLIONS, 1000);
		const expected = judged === "reopened by a widening" ? StreamCode.Old : StreamCode.NotFound;
		expect(settled(await next(reader))[3]).toBe(expected);
		expect(settled(await next(reader))).toEqual(["group", 1n + BILLIONS]);
		if (wide) await drain(clock, wide);
		const before = chained.fetches();

		if (judged === "reopened by a widening") reader.update({ delay: { max: null } });
		else await clock.advance(cache.DEFAULT_EXPIRY.mul(2));
		const after = await drain(clock, reader);
		expect(after.length).toBeLessThanOrEqual(1);
		expect(chained.fetches().up - before.up).toBeLessThanOrEqual(1);

		reader.close();
		wide?.close();
		await chained.close();
	});
}
