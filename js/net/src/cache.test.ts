import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as cache from "./cache.ts";
import { StreamCode, StreamError, TooFarBehind } from "./error.ts";
import { hooks, staleHole } from "./internal.ts";
import { RUN_OVERHEAD as ARRIVAL_OVERHEAD } from "./ledger.ts";
import { Tail } from "./tail.ts";
import { Producer as TrackProducer } from "./track.ts";

// A reader continued on a held re-offer keeps the frames the failed copy wrote that it has yet to
// read, so the pool still counts them once that copy leaves the cache.
test("a re-offer bills the failed copy's frames its readers still hold", async () => {
	await withClock(async () => {
		const pool = new cache.Pool({ capacity: 100 * billed(10_000) });
		const producer = new TrackProducer("test");
		hooks.poolTrack(producer, pool);
		const track = producer.accept({ maxAge: null });
		hooks.receiveTrack(track);
		const first = hooks.receiveGroup(track, { sequence: 0n, start: 0 });
		first.writeFrame({ payload: new Uint8Array(10_000) });
		const reader = first.consume();
		const arriving = hooks.arrivingGroup(track, 0n);
		first.abort(new Error("reset"));
		// The re-offer starts past the frame the reader still holds.
		hooks.receiveGroup(track, { sequence: 0n, start: 1 });
		arriving?.release();
		expect(pool.used).toBeGreaterThan(10_000);
		expect((await reader.readFrame())?.payload.byteLength).toBe(10_000);
		reader.close();
		track.close();
	});
});

// Run `fn` on a manual clock, stepped in milliseconds.
async function withClock(fn: (clock: { advance: (ms: number) => Promise<void> }) => Promise<void> | void) {
	const clock = new Manual();
	const installed = clock.install();
	try {
		await fn({ advance: (ms) => clock.advance(Duration.millis(ms)) });
	} finally {
		installed.close();
	}
}

// A track caching into `pool`, as a broadcast's tracks do.
function pooled(pool: cache.Pool, name = "test"): TrackProducer {
	const producer = new TrackProducer(name);
	hooks.poolTrack(producer, pool);
	return producer.accept({ maxAge: null });
}

function finished(producer: TrackProducer, size: number): bigint {
	const group = producer.appendGroup();
	group.writeFrame({ payload: new Uint8Array(size) });
	group.finish();
	return group.sequence;
}

// The sequences a fresh subscriber is handed: what the track still caches. Its mirrors are
// released once its close settles.
async function cached(producer: TrackProducer): Promise<bigint[]> {
	const subscriber = producer.subscribe({ delay: { max: null } });
	const sequences: bigint[] = [];
	for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup())
		sequences.push(group.sequence);
	subscriber.close();
	await yieldNow();
	return sequences;
}

// What one finished group of `size` payload bytes bills: its frame, the frame's own
// bookkeeping (group.ts `FRAME_BYTES`), and the group's.
const billed = (size: number) => size + 256 + cache.ENTRY_OVERHEAD;

// While the pool is over capacity, every append accrues debt and pays it by evicting the
// track's own oldest groups, so the newest content survives. Mirrors Rust `debt_evicts_oldest_group`.
test("debt evicts a track's oldest groups", async () => {
	await withClock(async () => {
		const pool = new cache.Pool({ capacity: 2 * billed(10_000) });
		const producer = pooled(pool);
		for (let i = 0; i < 4; i++) finished(producer, 10_000);

		const left = await cached(producer);
		expect(left).not.toContain(0n);
		expect(left).toContain(3n);
		// The protected live edge plus the group just demoted, since debt is charged before it.
		expect(pool.used).toBeLessThanOrEqual(2 * billed(10_000));
	});
});

// Far too small for one group, the pool still keeps the newest group whole. Mirrors Rust
// `latest_group_never_evicted`.
test("the newest group survives any capacity", async () => {
	await withClock(async () => {
		const pool = new cache.Pool({ capacity: 100 });
		const producer = pooled(pool);
		for (let i = 0; i < 3; i++) finished(producer, 1_000);
		const left = await cached(producer);
		expect(left).not.toContain(0n);
		expect(left).toContain(2n);
		const newest = producer.subscribe({ delay: { max: null }, ranges: [{ start: { absolute: 2n } }] });
		expect((await newest.tryRecvGroup()?.readFrame())?.payload.length).toBe(1_000);
	});
});

// A reader holding an evicted group's unread frames is told so, rather than hanging or
// reading a truncated clean end. Mirrors Rust `eviction_aborts_readers`.
test("eviction fails a reader's unread group with Evicted", async () => {
	await withClock(async () => {
		const pool = new cache.Pool({ capacity: billed(10_000) + cache.MIRROR_OVERHEAD });
		const producer = pooled(pool);
		const subscriber = producer.subscribe({ delay: { max: null } });
		finished(producer, 10_000); // seq 0
		const held = subscriber.tryRecvGroup();
		finished(producer, 10_000); // seq 1: demotes seq 0
		finished(producer, 10_000); // seq 2: pays by evicting seq 0
		await expect(held?.readFrame()).rejects.toMatchObject({ code: StreamCode.Evicted });
		subscriber.close();
	});
});

// Debt too small to evict an old group carries over rather than shedding the live group's
// frames, which its reader has not taken yet. Mirrors Rust `carried_debt_keeps_the_live_group`.
test("carried debt keeps the live group", async () => {
	await withClock(async () => {
		const unit = 100 * cache.ENTRY_OVERHEAD;
		const pool = new cache.Pool({ capacity: 22 * unit });
		const producer = pooled(pool);
		finished(producer, 20 * unit);

		const group = producer.appendGroup();
		const reader = group.consume();
		for (let i = 0; i < 3; i++) group.writeFrame({ payload: new Uint8Array(unit) });
		group.finish();
		for (let i = 0; i < 3; i++) expect((await reader.readFrame())?.payload.length).toBe(unit);
		expect(await reader.readFrame()).toBeUndefined();
	});
});

// A read lifts a group's access above the pool's mean, so the eviction walk passes over it
// and evicts a never-read group instead, even one that arrived later. Mirrors Rust
// `fetch_refresh_survives_eviction`.
test("a read group outlives a later unread one", async () => {
	await withClock(async (clock) => {
		// Each group is mirrored to the reader too.
		const pool = new cache.Pool({ capacity: Math.floor(3.5 * (billed(3_000) + cache.MIRROR_OVERHEAD)) });
		const producer = pooled(pool);
		const reader = producer.subscribe({ delay: { max: null } });

		finished(producer, 3_000); // seq 0
		await clock.advance(1_000);
		finished(producer, 3_000); // seq 1
		await clock.advance(1_000);
		finished(producer, 3_000); // seq 2
		await clock.advance(500);
		const first = reader.tryRecvGroup();
		expect(first?.sequence).toBe(0n);
		expect((await first?.readFrame())?.payload.length).toBe(3_000);
		await clock.advance(500);

		finished(producer, 3_000); // seq 3
		await clock.advance(1_000);
		finished(producer, 3_000); // seq 4
		reader.close();

		const left = await cached(producer);
		expect(left).toContain(0n);
		expect(left).not.toContain(1n);
	});
});

// Taking a group is a cache access: a reader handed one just inside the window can still read
// it almost a window later. Mirrors Rust `delivery_restarts_the_expiry_clock`.
test("delivery restarts a group's idle window", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(10_000) });
		const producer = pooled(pool);
		const subscriber = producer.subscribe({ delay: { max: null } });
		finished(producer, 1); // seq 0
		finished(producer, 1); // seq 1 takes the live edge, so seq 0 can expire

		await clock.advance(9_000);
		const reading = subscriber.tryRecvGroup();
		expect(reading?.sequence).toBe(0n);
		// Far beyond the write, inside the delivery. The new group runs the expiry scan.
		await clock.advance(9_000);
		finished(producer, 1);
		expect((await reading?.readFrame())?.payload.length).toBe(1);
		subscriber.close();
	});
});

// Every track of a broadcast caches into one pool, and the mean access across all of them
// orders eviction: a track writing past the capacity pays with its own groups, but only those
// accessed no later than the mean, so a fresher track's groups survive its own debt while a
// staler track's write evicts its oldest at double rate.
test("a shared capacity spares a fresher track and evicts a staler one's oldest", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ capacity: 6 * billed(10_000) });
		const open = (producer: TrackProducer) => {
			const group = producer.appendGroup();
			group.writeFrame({ payload: new Uint8Array(10_000) });
			return group;
		};
		const staler = pooled(pool, "staler");
		const old = [open(staler), open(staler), open(staler), open(staler)];

		await clock.advance(10_000);
		const fresher = pooled(pool, "fresher");
		const reader = fresher.subscribe({ delay: { max: null } });
		const read = [];
		for (let i = 0; i < 4; i++) {
			read.push(open(fresher));
			expect((await reader.tryRecvGroup()?.readFrame())?.payload.length).toBe(10_000);
		}
		expect(pool.used).toBeGreaterThan(6 * billed(10_000));
		for (const group of read) expect(group.closed.peek()).toBeUndefined();

		open(staler);
		// One write's worth of debt, doubled: two groups.
		expect(old[0].closed.peek()).toMatchObject({ code: StreamCode.Evicted });
		expect(old[1].closed.peek()).toMatchObject({ code: StreamCode.Evicted });
		expect(old[2].closed.peek()).toBeUndefined();
		for (const group of read) expect(group.closed.peek()).toBeUndefined();
		reader.close();
	});
});

// The pool's sweep reclaims every track's idle groups with no write behind it, the live edges
// excepted, and leaves only what those still bill. Mirrors Rust `pool_sweep_expires_without_a_write`.
test("the sweep expires idle groups across tracks without a write", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const tracks = [pooled(pool, "a"), pooled(pool, "b")];
		const stalled = tracks.map((track) => {
			const group = track.appendGroup(); // left open
			group.writeFrame({ payload: new Uint8Array(1) });
			finished(track, 1); // the live edge
			return group;
		});

		await clock.advance(1_000);
		for (const group of stalled) expect(group.closed.peek()).toBeUndefined();
		await clock.advance(1_000 + 500);
		for (const group of stalled) expect(group.closed.peek()).toBeInstanceOf(TooFarBehind);
		for (const track of tracks) expect(await cached(track)).toEqual([1n]);
		expect(pool.used).toBe(2 * billed(1));
	});
});

// Every byte a group bills, its subscribers' mirrors included, goes back once it leaves.
test("the pool is billed only for what is cached", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		const subscriber = producer.subscribe({ delay: { max: null } });
		finished(producer, 100);
		expect(pool.used).toBe(billed(100) + cache.MIRROR_OVERHEAD);
		subscriber.close();
		await yieldNow();
		expect(pool.used).toBe(billed(100));

		producer.finish();
		producer.close();
		await clock.advance(3_000);
		expect(pool.used).toBe(0);
	});
});

// The group's own tail limit still sheds its front, and the pool is credited for it.
test("a group's tail limit releases what it sheds from the pool", async () => {
	await withClock(() => {
		const pool = new cache.Pool();
		const producer = pooled(pool);
		const group = producer.appendGroup();
		const frame = new Uint8Array(1024 * 1024);
		for (let i = 0; i < 40; i++) group.writeFrame({ payload: frame });
		expect(hooks.groupCacheBytes(group)).toBeLessThanOrEqual(32 * 1024 * 1024);
		expect(pool.used).toBe(hooks.groupCacheBytes(group) + cache.ENTRY_OVERHEAD);
	});
});

// With nothing evictable, the debt sheds the live edge's front, keeping the newest frame, so
// an open group a track only appends frames to stays near the capacity.
test("the debt sheds the front of an open live edge", async () => {
	await withClock(async () => {
		const capacity = 1024 * 1024;
		const pool = new cache.Pool({ capacity });
		const producer = pooled(pool);
		const reader = producer.subscribe({ delay: { max: null } });
		const group = producer.appendGroup();
		const behind = reader.tryRecvGroup();
		const frame = new Uint8Array(64 * 1024);
		for (let i = 0; i < 64; i++) group.writeFrame({ payload: frame });

		expect(group.closed.peek()).toBeUndefined();
		// What a frame write may add before it settles, on top of the capacity.
		expect(pool.used).toBeLessThanOrEqual(capacity + 512 * 1024);
		// A reader that never read lost the shed front.
		await expect(behind?.readFrame()).rejects.toThrow();
	});
});

// Every run a fresh reader of `producer` is told was given up: what the track still holds.
function dropsHeld(producer: TrackProducer): bigint[] {
	const reader = producer.subscribe({ delay: { max: null } });
	const held: bigint[] = [];
	for (let run = hooks.tryDrop(reader); run; run = hooks.tryDrop(reader)) held.push(run.start);
	reader.close();
	return held;
}

const old = () => ({ error: new StreamError(StreamCode.Old) });

// A drop-only flood of disjoint runs bills the pool and sheds its lowest runs to stay within it,
// with no cached group to evict; a shed run is unknown again, so a reader reports only the runs
// the track still holds. Mirrors Rust `a_drop_flood_stays_within_the_pool`.
test("a drop flood stays within the pool", () => {
	const runs = 1000n;
	const capacity = 64 * cache.RUN_OVERHEAD;
	const pool = new cache.Pool({ capacity });
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({ delay: { max: null } });
	const reported: bigint[] = [];
	for (let run = 0n; run < runs; run++) {
		hooks.dropTrackGroups(producer, 2n * run, 2n * run + 1n, old());
		expect(pool.used).toBeLessThanOrEqual(capacity + 2 * cache.RUN_OVERHEAD);
		// Midway, the reader takes in every run held then and reports the lowest; the rest are
		// shed before it reports them.
		if (run === runs / 2n) {
			const first = hooks.tryDrop(reader);
			if (!first) throw new Error("a run is owed");
			reported.push(first.start);
		}
	}
	const used = pool.used;
	// A probe's record bills too: an unbounded pool lets it read every run without shedding one.
	pool.resize();
	const held = dropsHeld(producer);
	pool.resize(capacity);
	// Each drop is accounted in the arrivals too, whose gaps are folded before any run is shed.
	expect(hooks.arrivalRuns(producer)).toBeLessThanOrEqual(2);
	expect(used).toBe(held.length * cache.RUN_OVERHEAD + (hooks.arrivalRuns(producer) - 1) * ARRIVAL_OVERHEAD);
	expect(held[0]).toBeGreaterThan(0n);
	expect(held.every((start, i) => i === 0 || start === held[i - 1] + 2n)).toBe(true);
	expect(held[held.length - 1]).toBe(2n * (runs - 1n));
	for (let run = hooks.tryDrop(reader); run; run = hooks.tryDrop(reader)) reported.push(run.start);
	// Billing the reader's record shed the track's lowest runs before it reported them.
	pool.resize();
	expect(reported.slice(1)).toEqual(dropsHeld(producer));
});

// A flood a reader reported, fragmented into runs alternating with upstream skips, bills its
// record too. Once the pool shrinks under it and its track has shed what it can, the reader folds
// the final runs and the groups it read into its floor and stays within the pool, while one
// holding stale reports an update would reopen ends with TooFarBehind rather than forgo the
// re-offer. Mirrors Rust
// `a_reported_flood_folds_or_ends_the_reader`.
for (const stale of [false, true]) {
	test(`a reported flood ${stale ? "ends" : "folds into"} the reader`, () => {
		const runs = 1000n;
		const budget = 1000;
		const pool = new cache.Pool({ capacity: 1 << 30 });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		const reader = producer.subscribe({ delay: { max: Duration.millis(budget) } });
		const hole = stale ? staleHole(budget) : { error: new StreamError(StreamCode.Cancel) };
		const flood = (run: bigint) => {
			hooks.dropTrackGroups(producer, 2n * run, 2n * run + 1n, hole);
			producer.skipGroups(2n * run + 1n, 2n * run + 2n);
		};
		for (let run = 0n; run < runs; run++) flood(run);
		let reported = 0n;
		for (let run = hooks.tryDrop(reader); run; run = hooks.tryDrop(reader)) reported += run.end - run.start;
		expect(reported).toBe(2n * runs);
		expect(reader.tryRecvGroup()).toBeUndefined();
		expect(pool.used).toBeGreaterThanOrEqual(Number(2n * runs) * cache.READER_RUN_OVERHEAD);

		// The pool shrinks, and groups the reader reads above the flood accrue its debt: the track
		// sheds its own drop set first, then the reader pays.
		const capacity = 64 * cache.RUN_OVERHEAD;
		pool.resize(capacity);
		let error: unknown;
		for (let sequence = 2n * runs; sequence < 2n * runs + 64n && error === undefined; sequence++) {
			const group = producer.createGroup(sequence);
			group.writeFrame({ payload: new Uint8Array(16 * 1024) });
			group.finish();
			try {
				expect(reader.tryRecvGroup()?.sequence).toBe(sequence);
			} catch (err) {
				error = err;
			}
		}
		if (stale) expect(error).toBeInstanceOf(TooFarBehind);
		else {
			expect(error).toBeUndefined();
			expect(hooks.tryDrop(reader)).toBeUndefined();
		}
		expect(pool.used).toBeLessThanOrEqual(capacity + 2 * (16 * 1024 + cache.ENTRY_OVERHEAD));
		reader.close();
	});
}

// A reader that closes while a read waits releases its record's bill for good: the read that wakes
// after the close bills nothing again.
test("a closed reader bills nothing from a read still pending", async () => {
	const pool = new cache.Pool({ capacity: 1 << 30 });
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({ delay: { max: null } });
	for (let run = 0n; run < 64n; run++) {
		hooks.dropTrackGroups(producer, 2n * run, 2n * run + 1n, { error: new StreamError(StreamCode.Cancel) });
		producer.skipGroups(2n * run + 1n, 2n * run + 2n);
	}
	while (hooks.tryDrop(reader));
	const billed = pool.used;
	const read = reader.recvGroup();
	reader.close();
	await read;
	expect(pool.used).toBeLessThan(billed - 32 * cache.READER_RUN_OVERHEAD);
});

// A subscription's tail, recording a fragmented run of finished streams between updates, bills it to
// its track's account. Under the pool's debt it forgets them and stays within the pool. Mirrors Rust
// `a_tail_forgets_its_finished_streams_under_debt`.
test("a tail forgets its finished streams under debt", () => {
	const runs = 4000n;
	const pool = new cache.Pool({ capacity: 1 << 30 });
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const tail = new Tail({ bill: hooks.billTrack(producer) });
	for (let run = 0n; run < runs; run++) tail.finish(2n * run);
	expect(tail.unfinished(0n, 2n * runs).length).toBe(Number(runs));
	expect(pool.used).toBeGreaterThanOrEqual(Number(runs) * cache.READER_RUN_OVERHEAD);

	// The pool shrinks, and groups written above them accrue the track's debt, which its own groups
	// cannot pay: the tail pays it by forgetting what finished.
	const capacity = 64 * cache.RUN_OVERHEAD;
	pool.resize(capacity);
	for (let i = 0; i < 64; i++) finished(producer, 16 * 1024);
	tail.finish(2n * runs);
	expect(tail.unfinished(0n, 2n * runs + 1n).length).toBeLessThanOrEqual(2);
	expect(pool.used).toBeLessThanOrEqual(capacity + 2 * (16 * 1024 + cache.ENTRY_OVERHEAD));
	// A stale drop inside what the fold forgot is owed again only where the ledger had not accounted
	// for it: there a stream may have finished and overtaken the drop.
	tail.open(2n).done();
	expect(tail.overtaken(0n, 4n)).toEqual([{ start: 2n, end: 3n }]);
	expect(tail.overtaken(2n * runs, 2n * runs + 1n)).toEqual([]);
	tail.updated();
	expect(tail.overtaken(0n, 4n)).toEqual([]);
	tail.close();
});

// Whatever a bill frees pays its readers' debt, however it shrank, and the debt is forgiven once
// the pool is back within its capacity. A closed tail never bills again. Mirrors Rust
// `a_debt_is_paid_by_any_shrink_and_forgiven_without_pressure`.
test("a reader's debt is paid by any shrink and forgiven without pressure", () => {
	const unit = cache.READER_RUN_OVERHEAD;
	const pool = new cache.Pool({ capacity: 1 << 30 });
	const producer = pooled(pool);
	const bill = hooks.billTrack(producer);
	bill.set(4 * unit);
	pool.resize(2 * unit);
	// Nothing else is cached, so the debt the track accrues falls to its readers.
	producer.createGroup(0n).abort(new Error("gone"));
	bill.set(5 * unit);
	const owed = bill.debt;
	expect(owed).toBeGreaterThan(unit);
	bill.set(4 * unit);
	expect(bill.debt).toBe(owed - unit);
	pool.resize(1 << 30);
	expect(bill.debt).toBe(0);
	pool.resize(2 * unit);
	expect(bill.debt).toBe(0);
	bill.close();

	const tail = new Tail({ bill: hooks.billTrack(producer) });
	tail.finish(0n);
	tail.finish(2n);
	tail.close();
	const used = pool.used;
	tail.finish(4n);
	expect(pool.used).toBe(used);
});

// A peer fragmenting its arrivals under a grace that never folds a gap bills each run to the pool,
// which folds the oldest gaps early to stay within it. Mirrors Rust `an_arrival_flood_stays_within_the_pool`.
test("an arrival flood stays within the pool", () => {
	const groups = 1000n;
	const capacity = 64 * ARRIVAL_OVERHEAD;
	const pool = new cache.Pool({ capacity });
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	// A reader that waits for a late group indefinitely, so no gap folds by its grace.
	const reader = producer.subscribe({ delay: { max: null } });
	for (let group = 0n; group < groups; group++) producer.createGroup(2n * group).finish();
	const runs = hooks.arrivalRuns(producer);
	expect(runs).toBeGreaterThan(1);
	expect(runs).toBeLessThan(Number(groups));
	expect(pool.used).toBeLessThanOrEqual(capacity + 2 * cache.ENTRY_OVERHEAD);
	reader.close();
});

// A frame write that folds an arrival gap to pay its debt wakes a reader parked on that gap, which
// has no grace timer to wake it.
test("folding an arrival gap wakes the reader parked on it", async () => {
	const pool = new cache.Pool({ capacity: 1 });
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({
		delay: { max: null },
		ranges: [{ start: { absolute: 0n }, end: { absolute: 3n } }],
	});
	// Group 0 is dropped rather than cached, so the protected live edge is the only group and the
	// gap below it is the first thing a payment takes: no other signal moves.
	hooks.dropTrackGroups(producer, 0n, 1n, old());
	const live = producer.createGroup(2n);
	live.writeFrame({ payload: new Uint8Array(8) });
	for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup());
	for (let run = hooks.tryDrop(reader); run; run = hooks.tryDrop(reader));
	let woken = 0;
	const dispose = hooks.groupChanged(reader, () => woken++);
	expect(hooks.arrivalRuns(producer)).toBe(2);
	// Past the write threshold that settles debt.
	live.writeFrame({ payload: new Uint8Array(512 * 1024) });
	expect(hooks.arrivalRuns(producer)).toBe(1);
	await yieldNow();
	expect(woken).toBeGreaterThan(0);
	dispose();
	reader.close();
});

// A closed track bills none of its arrival gaps, so an account the pool's sweep keeps for its drops
// holds no bytes for them.
test("a closed track releases its arrival gaps from the pool", async () => {
	await withClock(() => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		const reader = producer.subscribe({ delay: { max: null } });
		producer.createGroup(0n).finish();
		producer.createGroup(2n).finish();
		expect(hooks.arrivalRuns(producer)).toBe(2);
		const used = pool.used;
		producer.close();
		expect(pool.used).toBe(used - ARRIVAL_OVERHEAD);
		reader.close();
	});
});

// A run given up longer than the pool's idle window ago is forgotten by the sweep, even on a
// track that caches nothing, and a run given up since stays. Mirrors Rust
// `the_sweep_forgets_runs_past_the_expiry_window`.
test("the sweep forgets runs past the idle window", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		// A local track keeps what it skipped above its newest group, which its end relies on.
		const local = pooled(pool, "local");
		local.skipGroups(5n, 10n);
		hooks.dropTrackGroups(producer, 0n, 1n, old());
		hooks.dropTrackGroups(producer, 2n, 3n, old());
		await clock.advance(500);
		hooks.dropTrackGroups(producer, 4n, 5n, old());
		// Each run and its expiry are billed, and so are the two gaps between the drops the
		// arrivals account.
		expect(pool.used).toBe(4 * (cache.RUN_OVERHEAD + cache.BORN_OVERHEAD) + 2 * ARRIVAL_OVERHEAD);

		// The sweep at 1.5 s forgets the first two, given up at 0 s.
		await clock.advance(1_000);
		expect(dropsHeld(producer)).toEqual([4n]);
		expect(pool.used).toBe(2 * (cache.RUN_OVERHEAD + cache.BORN_OVERHEAD));

		await clock.advance(1_000);
		expect(dropsHeld(producer)).toEqual([]);
		expect(dropsHeld(local)).toEqual([5n]);
		expect(pool.used).toBe(cache.RUN_OVERHEAD + cache.BORN_OVERHEAD);
	});
});

// A local skip kept above the track's edge is dated once the edge passes it, or once the track
// closes, so an expiry scan past its window forgets it with no other drop to rearm it, and its
// bill leaves the pool. Mirrors Rust `a_kept_local_skip_expires_once_the_edge_passes_it`; Rust
// frees a closed track's drops with its state.
test("a kept local skip expires once the edge passes it or the track closes", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const passed = pooled(pool);
		const closed = pooled(pool, "closed");
		passed.skipGroups(0n, 10n);
		closed.skipGroups(0n, 10n);
		await clock.advance(3_000);
		expect(dropsHeld(passed)).toEqual([0n]);
		expect(dropsHeld(closed)).toEqual([0n]);

		// The write's own expiry scan finds the skip.
		passed.createGroup(10n).finish();
		expect(dropsHeld(passed)).toEqual([]);

		const used = pool.used;
		closed.close();
		expect(pool.used).toBe(used - cache.RUN_OVERHEAD - cache.BORN_OVERHEAD);
	});
});

// A final drop ends a group still arriving, but leaves one that finished here to every reader, as a
// group arriving after its drop replaces it. Mirrors Rust `a_final_drop_leaves_a_finished_group`.
test("a final drop leaves a finished group", async () => {
	const producer = pooled(new cache.Pool({ capacity: 1 << 30 }));
	hooks.receiveTrack(producer);
	producer.createGroup(0n).finish();
	const open = producer.createGroup(1n);
	hooks.dropTrackGroups(producer, 0n, 2n, { error: new StreamError(StreamCode.Cancel) });

	expect(dropsHeld(producer)).toEqual([1n]);
	expect(open.closed.peek()).toBeInstanceOf(Error);
	expect(await cached(producer)).toContain(0n);
});

// A received stale run the pool forgets is accounted for, as an evicted group is: no reader waits
// on it from the live feed, so a reader that wants it asks a fill. Mirrors Rust
// `a_reader_asks_a_fill_for_a_forgotten_run_it_judged`.
test("a forgotten stale run is asked of a fill", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		const requests = producer.requests();
		producer.createGroup(0n).finish();
		hooks.dropTrackGroups(producer, 1n, 3n, staleHole(500));
		producer.createGroup(3n).finish();
		await clock.advance(2_000);

		const reader = producer.subscribe({
			ranges: [{ start: { absolute: 1n } }],
			delay: { max: Duration.millis(60_000) },
			order: "ascending",
		});
		// The live group first, in arrival order; the next read asks for what it misses.
		expect((await reader.recvGroup())?.sequence).toBe(3n);
		const next = reader.recvGroup();
		const asked: bigint[][] = [];
		for (;;) {
			const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
			if (!fill) break;
			asked.push([fill.start, fill.end]);
		}
		expect(asked).toEqual([[1n, 3n]]);

		reader.close();
		requests.close();
		await next.catch(() => undefined);
	});
});

// A failed fill answer's drop accounts nothing the feed owes, even once the pool forgets it: the gap
// stays owed until the feed delivers it or the tail's grace passes, as Rust records no drop for it.
test("a forgotten failed fill answer leaves its gap owed", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
		for (const sequence of [0n, 2n]) hooks.receiveGroup(producer, { sequence, start: 0 }).finish();
		hooks.receiveGroup(producer, { sequence: 1n, start: 0, fill: true }).abort(new Error("archive failed"));
		hooks.endTrack(producer, { group: 3n, frame: 0 });
		await yieldNow();
		// A failed fill answer says nothing of the feed, so it records no drop, as in Rust.
		expect(dropsHeld(producer)).toEqual([]);
		expect(producer.closed.peek()).toBeUndefined();

		await clock.advance(3_000);
		expect(dropsHeld(producer)).toEqual([]);
		expect(producer.closed.peek()).toBeUndefined();
		reader.close();
	});
});

// A failed fill answer a reader still holds stays billed, and reclaimable by the pool, while the
// reader keeps its frames: it leaves delivery, not the cache, as Rust keeps a spent fetch-only slot
// until its scan reclaims it.
test("a held failed fill answer stays billed", async () => {
	const pool = new cache.Pool({});
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	const answer = hooks.receiveGroup(producer, { sequence: 1n, start: 0, fill: true });
	answer.writeFrame({ payload: new Uint8Array(10_000) });
	const group = await reader.recvGroup();
	expect(group?.sequence).toBe(1n);
	answer.abort(new Error("archive failed"));
	await yieldNow();
	expect(pool.used).toBeGreaterThanOrEqual(10_000);
	expect((await group?.readFrame())?.payload.byteLength).toBe(10_000);
	reader.close();
});

// A received copy that reached its end through a skip stays ended once the pool forgets the skip,
// since the forgotten run stays accounted for. Mirrors Rust
// `a_reached_end_outlives_its_expired_drops`.
test("a reached end outlives its forgotten skip", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const producer = pooled(pool);
		hooks.receiveTrack(producer);
		producer.createGroup(0n).finish();
		hooks.skipTrackGroups(producer, 1n, 3n);
		hooks.endTrack(producer, { group: 3n, frame: 0 });
		const done = (reader: ReturnType<TrackProducer["subscribe"]>) => {
			for (let recv = hooks.tryRecvGroup(reader, false); ; recv = hooks.tryRecvGroup(reader, false)) {
				if (recv.kind !== "group") return recv.kind;
				recv.group.close();
			}
		};
		const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
		expect(done(reader)).toBe("done");

		await clock.advance(3_000);
		expect(dropsHeld(producer)).toEqual([]);
		expect(done(reader)).toBe("done");
		const late = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
		expect(done(late)).toBe("done");
		reader.close();
		late.close();
	});
});
