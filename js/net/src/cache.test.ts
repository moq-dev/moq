import { expect, test } from "bun:test";
import { Manual, Milli, Nano, yieldNow } from "@moq/clock";
import * as cache from "./cache.ts";
import { StreamCode, TooFarBehind } from "./error.ts";
import { hooks } from "./internal.ts";
import { Producer as TrackProducer } from "./track.ts";

// Run `fn` on a manual clock, stepped in milliseconds.
async function withClock(fn: (clock: { advance: (ms: number) => Promise<void> }) => Promise<void> | void) {
	const clock = new Manual();
	const installed = clock.install();
	try {
		await fn({ advance: (ms) => clock.advance(Nano.fromMilli(Milli(ms))) });
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
		const pool = new cache.Pool({ expiry: Milli(10_000) });
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
		const pool = new cache.Pool({ expiry: Milli(1_000) });
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
		const pool = new cache.Pool({ expiry: Milli(1_000) });
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

// A group leaving the cache leaves nothing in the eviction order holding its frames, so a track
// that stops publishing does not pin what already expired.
test("expired groups are not retained by the eviction order", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Milli(1_000) });
		const producer = pooled(pool);
		const refs: WeakRef<object>[] = [];
		for (let i = 0; i < 8; i++) {
			const group = producer.appendGroup();
			group.writeFrame({ payload: new Uint8Array(1) });
			group.finish();
			refs.push(new WeakRef(group));
		}
		await clock.advance(3_000);
		expect(await cached(producer)).toEqual([7n]);
		Bun.gc(true);
		expect(refs.slice(0, 7).filter((ref) => ref.deref() !== undefined)).toEqual([]);
	});
});
