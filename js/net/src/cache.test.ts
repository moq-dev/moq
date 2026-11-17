import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as cache from "./cache.ts";
import { StreamCode, StreamError, TooFarBehind } from "./error.ts";
import { hooks, staleHole } from "./internal.ts";
import { groupsOf } from "./mock.ts";
import { Timestamp } from "./time.ts";
import { Producer as TrackProducer } from "./track.ts";

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
	const group = hooks.appendNext(producer);
	group.writeFrame({ payload: new Uint8Array(size), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	return group.start;
}

// The sequences a fresh subscriber is handed: what the track still caches. Its mirrors are
// released once its close settles.
async function cached(producer: TrackProducer): Promise<bigint[]> {
	const subscriber = producer.subscribe({ delay: { max: null } });
	const sequences: bigint[] = [];
	for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup())
		sequences.push(group.start);
	subscriber.close();
	await yieldNow();
	return sequences;
}

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
		expect(reading?.start).toBe(0n);
		// Far beyond the write, inside the delivery. The new group runs the expiry scan.
		await clock.advance(9_000);
		finished(producer, 1);
		expect((await reading?.readFrame())?.payload.length).toBe(1);
		subscriber.close();
	});
});

// The pool's sweep reclaims every track's idle groups with no write behind it, the live edges
// excepted. Mirrors Rust `pool_sweep_expires_without_a_write`.
test("the sweep expires idle groups across tracks without a write", async () => {
	await withClock(async (clock) => {
		const pool = new cache.Pool({ expiry: Duration.millis(1_000) });
		const tracks = [pooled(pool, "a"), pooled(pool, "b")];
		const stalled = tracks.map((track) => {
			const group = hooks.appendNext(track); // left open
			group.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.unscaled(group.start) });
			finished(track, 1); // the live edge
			return group;
		});

		await clock.advance(1_000);
		for (const group of stalled) expect(group.closed.peek()).toBeUndefined();
		await clock.advance(1_000 + 500);
		for (const group of stalled) expect(group.closed.peek()).toBeInstanceOf(TooFarBehind);
		for (const track of tracks) expect(await cached(track)).toEqual([1n]);
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

		// The sweep at 1.5 s forgets the first two, given up at 0 s.
		await clock.advance(1_000);
		expect(dropsHeld(producer)).toEqual([4n]);

		await clock.advance(1_000);
		expect(dropsHeld(producer)).toEqual([]);
		expect(dropsHeld(local)).toEqual([5n]);
	});
});

// A local skip kept above the track's edge is dated once the edge passes it, or once the track
// closes, so an expiry scan past its window forgets it with no other drop to rearm it. Mirrors Rust `a_kept_local_skip_expires_once_the_edge_passes_it`; Rust
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

		closed.close();
		await clock.advance(1_500);
		expect(dropsHeld(closed)).toEqual([]);
	});
});

// A final drop ends a group still arriving, but leaves one that finished here to every reader, as a
// group arriving after its drop replaces it. Mirrors Rust `a_final_drop_leaves_a_finished_group`.
test("a final drop leaves a finished group", async () => {
	const producer = pooled(new cache.Pool());
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
			ranges: [{ start: { time: 1n } }],
			delay: { max: Duration.millis(60_000) },
			order: "ascending",
		});
		// The live group first, in arrival order; the next read asks for what it misses.
		expect((await reader.recvGroup())?.start).toBe(3n);
		const next = reader.recvGroup();
		const asked: bigint[][] = [];
		for (;;) {
			const fill = groupsOf(await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]));
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
		for (const sequence of [0n, 2n]) hooks.receiveGroup(producer, { start: sequence, offset: 0 }).finish();
		hooks.receiveGroup(producer, { start: 1n, offset: 0, fill: true }).abort(new Error("archive failed"));
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

// A failed fill answer a reader still holds stays readable, and reclaimable by the pool: it leaves
// delivery, not the cache, as Rust keeps a spent fetch-only slot until its scan reclaims it.
test("a held failed fill answer stays readable", async () => {
	const pool = new cache.Pool({});
	const producer = pooled(pool);
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	const answer = hooks.receiveGroup(producer, { start: 1n, offset: 0, fill: true });
	answer.writeFrame({ payload: new Uint8Array(10_000), timestamp: Timestamp.unscaled(answer.start) });
	const group = await reader.recvGroup();
	expect(group?.start).toBe(1n);
	answer.abort(new Error("archive failed"));
	await yieldNow();
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
