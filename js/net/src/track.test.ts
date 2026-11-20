import { expect, spyOn, test } from "bun:test";
import { Duration, Instant, Wall, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { race } from "@moq/signals";
import { DEFAULT_EXPIRY } from "./cache.ts";
import { ProtocolViolation, Skipped, StreamCode, StreamError, TimestampMismatch, TooFarBehind } from "./error.ts";
import type { Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import { hooks, maxDelayOf, skipHole, staleHole } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { rescale, rescaleIn } from "./rescale.ts";
import { Timescale, Timestamp } from "./time.ts";
import {
	type Anchor,
	authoredInfo,
	type Delay,
	type Order,
	type Range,
	Subscription,
	Producer as TrackProducer,
} from "./track.ts";
import { unwanted } from "./util/demand.ts";
import { LIVE, rangesStart, union } from "./window.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

const CACHE_WINDOW_MS = DEFAULT_EXPIRY.asMillis();
// How late the pool's sweep may reclaim a group nothing writes to: it runs every half window.
const SWEEP_LAG_MS = CACHE_WINDOW_MS / 2;

test("a cached transport tail covers only positions it actually holds", async () => {
	const producer = new TrackProducer("test");
	const subscriber = producer.subscribe();
	const tail = hooks.receiveGroup(producer, { start: 0n, offset: 5 });
	tail.writeFrame({ payload: new Uint8Array([7]), timestamp: Timestamp.unscaled(tail.start) });
	tail.finish();
	expect(hooks.cachedGroup(subscriber, 0n)).toBeUndefined();
	const covered = hooks.cachedGroup(subscriber, 0n, 5);
	expect((await covered?.readFrameIndex())?.index).toBe(5);
	covered?.close();
	// Distinct hostile starts cannot create one cache copy per request.
	for (let start = 6; start < 106; start++)
		expect(() => hooks.receiveGroup(producer, { start: 0n, offset: start })).toThrow("duplicate group");
	const retained = hooks.cachedGroup(subscriber, 0n, 5);
	expect((await retained?.readFrameIndex())?.payload).toEqual(new Uint8Array([7]));
	retained?.close();
	subscriber.close();
	producer.finish();
	producer.close();
});

/** Let every pending microtask and timer callback run, so a parked read gets a turn. */
function settle(): Promise<void> {
	return yieldNow();
}

// A manual clock reading `initial` milliseconds. `set` moves it to an absolute time and fires
// what came due; `advance` also drains what those timers schedule.
function manualTime(initial: number) {
	const clock = new Manual();
	const installed = clock.install();
	const to = (ms: number) => Instant.ZERO.saturatingAdd(Duration.millis(ms)).since(clock.now());
	clock.jump(to(initial));
	return {
		set: (ms: number) => clock.jump(to(ms)),
		advance: (ms: number) => clock.advance(to(ms)),
		stepWall: (ms: number) => clock.stepWall(Wall.unixMillis(ms)),
		installed,
	};
}

// A session-fed copy owes holes; an in-process producer does not. Received without a max age,
// it has no publisher limit, as a decoded TRACK_INFO without one does.
function receivedTrack(info: Parameters<TrackProducer["accept"]>[0] = {}) {
	const producer = new TrackProducer("test").accept({ maxAge: null, ...info });
	hooks.receiveTrack(producer);
	return producer;
}

function putGroup(producer: TrackProducer, key: bigint, millis?: number) {
	const group = producer.createGroup(key);
	if (millis !== undefined) group.writeFrame(textFrame("x", Timestamp.fromMillis(millis)));
	group.finish();
}

// Append the next group, a finished one-frame group presenting at `millis`, as the Rust
// `append_at` does: keys count up from 0 whatever the media time says. Returns its key.
function appendAt(producer: TrackProducer, millis: number, text = "x"): bigint {
	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame(text, Timestamp.fromMillis(millis)));
	group.finish();
	return group.start;
}

// Append the next group presenting at its key in milliseconds, as the Rust `append_frozen`
// does: a timeline that barely advances, since starts strictly increase and can't stop.
function appendFrozen(producer: TrackProducer, text = "x"): bigint {
	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame(text, Timestamp.fromMillis(Number(group.start))));
	group.finish();
	return group.start;
}

function drainOrdered(reader: import("./track.ts").Ordered) {
	const keys: bigint[] = [];
	for (let group = reader.tryNextGroup(); group; group = reader.tryNextGroup()) {
		keys.push(group.start);
		group.close();
	}
	return keys;
}

test("ordered reads wait only for subscription-owed groups", () => {
	const clock = manualTime(0);
	try {
		for (const received of [false, true]) {
			const producer = received ? receivedTrack() : new TrackProducer("test");
			const reader = producer.subscribe({ delay: { max: Duration.millis(1000) } }).ordered();
			putGroup(producer, 0n);
			putGroup(producer, 2n);
			expect(drainOrdered(reader)).toEqual(received ? [0n] : [0n, 2n]);
			putGroup(producer, 1n);
			expect(drainOrdered(reader)).toEqual(received ? [1n, 2n] : []);
			reader.close();
			producer.finish();
			producer.close();
		}
	} finally {
		clock.installed.close();
	}
});

test("an ordered owed hole wakes exactly at its wall budget", async () => {
	const clock = manualTime(0);
	const producer = receivedTrack();
	const reader = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
	try {
		putGroup(producer, 0n);
		expect(drainOrdered(reader)).toEqual([0n]);
		putGroup(producer, 2n);
		let resolved = false;
		const pending = reader.nextGroup().then((group) => {
			resolved = true;
			return group;
		});
		await clock.advance(49);
		expect(resolved).toBe(false);
		await clock.advance(50);
		expect(resolved).toBe(true);
		expect((await pending)?.start).toBe(2n);
		for (let key = 3n; key < 10n; key++) {
			putGroup(producer, key);
			expect(drainOrdered(reader)).toEqual([key]);
		}
	} finally {
		reader.close();
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("an ordered read skipping a stale group stops waiting on holes below it", () => {
	const clock = manualTime(0);
	const producer = receivedTrack();
	const reader = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
	// A wider subscriber keeps the ledger's gaps open past this reader's budget.
	const wide = producer.subscribe({ delay: { max: Duration.millis(1000) } });
	try {
		putGroup(producer, 0n);
		expect(drainOrdered(reader)).toEqual([0n]);
		putGroup(producer, 3n);
		// Group 2 arrives late: its successor makes it stale at 50 ms, while hole 1,
		// succeeded by group 2, is spared until 90 ms.
		clock.set(40);
		putGroup(producer, 2n);
		expect(drainOrdered(reader)).toEqual([]);
		// Skipping group 2 moves the cursor past hole 1, so group 3 waits on nothing.
		clock.set(60);
		expect(drainOrdered(reader)).toEqual([3n]);
	} finally {
		reader.close();
		wide.close();
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("an ordered owed hole follows media time and a lower successor", async () => {
	const clock = manualTime(0);
	const producer = receivedTrack({ timescale: Timescale.MILLI });
	const reader = producer.subscribe({ delay: { max: Duration.millis(1000) } }).ordered();
	try {
		putGroup(producer, 0n, 0);
		expect(drainOrdered(reader)).toEqual([0n]);
		const edge = producer.createGroup(4n);
		edge.writeFrame(textFrame("edge", Timestamp.fromMillis(4000)));
		expect(drainOrdered(reader)).toEqual([]);
		let resolved = false;
		const pending = reader.nextGroup().then((group) => {
			resolved = true;
			return group;
		});
		// The nearer successor puts group 1's reach behind the edge immediately.
		putGroup(producer, 2n, 2000);
		await settle();
		expect(resolved).toBe(true);
		expect((await pending)?.start).toBe(2n);
		expect(drainOrdered(reader)).toEqual([]); // Group 3 is still owed.
		edge.writeFrame(textFrame("within", Timestamp.fromMillis(4999)));
		expect(drainOrdered(reader)).toEqual([]);
		const deadline = reader.nextGroup();
		edge.writeFrame(textFrame("deadline", Timestamp.fromMillis(5000)));
		await settle();
		expect((await deadline)?.start).toBe(4n);
		edge.finish();
	} finally {
		reader.close();
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("an ordered owed hole wakes when its ledger gap folds", async () => {
	const clock = manualTime(0);
	const producer = receivedTrack();
	const reader = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
	try {
		putGroup(producer, 0n);
		expect(drainOrdered(reader)).toEqual([0n]);
		const opener = producer.createGroup(2n);
		await clock.advance(40);
		putGroup(producer, 3n);
		opener.abort(new StreamError(StreamCode.Cancel));
		let resolved = false;
		const pending = reader.nextGroup().then((group) => {
			resolved = true;
			return group;
		});
		await clock.advance(49);
		expect(resolved).toBe(false);
		await clock.advance(50);
		expect(resolved).toBe(true);
		expect((await pending)?.start).toBe(2n);
		expect(drainOrdered(reader)).toEqual([3n]);
	} finally {
		reader.close();
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("ordered owed holes follow zero, narrowed, unlimited, and publisher budgets", async () => {
	const clock = manualTime(0);
	try {
		for (const maxDelay of [Duration.ZERO, Duration.millis(1000), null]) {
			const producer = receivedTrack();
			const reader = producer.subscribe({ delay: { max: maxDelay } }).ordered();
			putGroup(producer, 0n);
			expect(drainOrdered(reader)).toEqual([0n]);
			putGroup(producer, 2n);
			expect(drainOrdered(reader)).toEqual(maxDelay?.isZero() ? [2n] : []);
			if (maxDelay === null) {
				await clock.advance(3_600_000);
				expect(drainOrdered(reader)).toEqual([]);
				putGroup(producer, 1n);
				expect(drainOrdered(reader)).toEqual([1n, 2n]);
			} else if (!maxDelay.isZero()) {
				const pending = reader.nextGroup();
				reader.update({ delay: { max: Duration.ZERO } });
				expect((await pending)?.start).toBe(2n);
			}
			reader.close();
			producer.finish();
			producer.close();
		}
		const producer = receivedTrack({ maxAge: Duration.millis(50) });
		const reader = producer.subscribe({ delay: { max: null } }).ordered();
		putGroup(producer, 0n);
		putGroup(producer, 2n);
		expect(drainOrdered(reader)).toEqual([0n]);
		const pending = reader.nextGroup();
		await clock.advance(3_600_050);
		expect((await pending)?.start).toBe(2n);
		reader.close();
		producer.finish();
		producer.close();
	} finally {
		clock.installed.close();
	}
});

test("wanted reflects subscriber demand and clears when the last one leaves", async () => {
	const producer = new TrackProducer("test");

	// No subscribers: no demand.
	expect(producer.demand().wanted.peek()).toBe(false);

	const a = producer.subscribe();
	const b = producer.subscribe().ordered();
	expect(producer.demand().wanted.peek()).toBe(true);

	// Closing one of two keeps demand.
	a.close();
	expect(producer.demand().wanted.peek()).toBe(true);

	// Closing the last subscriber drops demand. The consumer wire awaits this
	// to tear an idle upstream down.
	b.close();
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBe(false);
});

test("a producer never self-closes on zero demand: a publisher keeps serving new subscribers", async () => {
	const producer = new TrackProducer("video");

	// Demand comes and goes...
	const a = producer.subscribe();
	a.close();
	await unwanted(producer.demand());

	// ...but the producer stays open (only the wire acts on unwanted demand, not the producer),
	// so a publisher writing to a track nobody is watching is unaffected.
	expect(producer.closed.peek()).toBeUndefined();

	// A later subscriber still works and replays the cache.
	producer.writeFrame(textFrame("still here", Timestamp.unscaled(0n)));
	const b = producer.subscribe().ordered();
	expect(await b.readString()).toBe("still here");
	expect(producer.demand().wanted.peek()).toBe(true);
});

test("unwanted resolves immediately when there was never any demand", async () => {
	const producer = new TrackProducer("video");
	// No subscriber was ever attached; the wait must not hang.
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBe(false);
});

test("wanted stays set across churn while at least one subscriber remains", async () => {
	const producer = new TrackProducer("video");

	const a = producer.subscribe();
	// Rapidly add and drop extra subscribers; `a` keeps demand asserted throughout.
	for (let i = 0; i < 20; i++) {
		const t = producer.subscribe();
		t.close();
		expect(producer.demand().wanted.peek()).toBe(true);
	}

	a.close();
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBe(false);
});

// Append a group of a 3 byte first frame and a `delta` byte delta to an untimed track.
function refreshGroup(producer: TrackProducer, delta: number): void {
	const group = hooks.appendNext(producer);
	group.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.unscaled(group.start) });
	if (delta > 0) group.writeFrame({ payload: new Uint8Array(delta), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
}

// The group start the producer's readers ask for while it is past every key written.
function asked(producer: TrackProducer): bigint | undefined {
	return hooks.demandAsked(producer.demand()).peek();
}

// Whether the producer's readers ask for a new group now.
function refreshing(producer: TrackProducer): boolean {
	return producer.demand().refresh.peek();
}

test("wanted follows the first consumer", async () => {
	const producer = new TrackProducer("test").accept({ refresh: { ratio: 1 } });
	expect(producer.demand().wanted.peek()).toBe(false);
	expect(refreshing(producer)).toBe(false);

	const subscriber = producer.subscribe();
	expect(producer.demand().wanted.peek()).toBe(true);
	expect(refreshing(producer)).toBe(true);
	refreshGroup(producer, 0);
	expect(refreshing(producer)).toBe(false);

	subscriber.close();
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBe(false);
});

// The cached group is served either way: refresh is a request, not a hold.
test("a join over the refresh ratio wants the group after the newest", async () => {
	for (const [delta, wanted] of [
		[0, undefined],
		[3, undefined],
		[4, 1n],
	] as const) {
		const producer = new TrackProducer("test").accept({ refresh: { ratio: 1 } });
		refreshGroup(producer, delta);
		const subscriber = producer.subscribe();
		expect(asked(producer)).toBe(wanted);
		expect(refreshing(producer)).toBe(wanted !== undefined);
		expect((await subscriber.recvGroup())?.start).toBe(0n);
		subscriber.close();
	}
});

test("joins coalesce until the group lands", () => {
	const producer = new TrackProducer("test").accept({ refresh: { ratio: 1 } });
	refreshGroup(producer, 4);
	const subscribers = [producer.subscribe()];
	expect(asked(producer)).toBe(1n);
	subscribers.push(producer.subscribe());
	expect(asked(producer)).toBe(1n);

	// Group 1 answers it, and a join under its ratio asks nothing more.
	refreshGroup(producer, 0);
	expect(asked(producer)).toBeUndefined();
	subscribers.push(producer.subscribe());
	expect(asked(producer)).toBeUndefined();

	refreshGroup(producer, 4);
	subscribers.push(producer.subscribe());
	expect(asked(producer)).toBe(3n);
	expect(refreshing(producer)).toBe(true);
	for (const subscriber of subscribers) subscriber.close();
});

test("joins without refresh ask for no group", () => {
	const producer = new TrackProducer("test").accept({});
	refreshGroup(producer, 1000);
	const subscriber = producer.subscribe();
	expect(asked(producer)).toBeUndefined();
	subscriber.close();
});

test("a fixed start is no join", () => {
	const producer = new TrackProducer("test").accept({ refresh: { ratio: 1 } });
	refreshGroup(producer, 4);
	const subscriber = producer.subscribe({ ranges: [{ start: { time: 0n } }] });
	expect(asked(producer)).toBeUndefined();
	subscriber.close();
});

test("a refresh without an interval is judged by the ratio", () => {
	const producer = new TrackProducer("test").accept({ refresh: { ratio: 1 } });
	const subscriber = producer.subscribe();
	refreshGroup(producer, 3);
	void subscriber.refresh();
	expect(asked(producer)).toBeUndefined();
	refreshGroup(producer, 4);
	void subscriber.refresh();
	expect(asked(producer)).toBe(2n);
	subscriber.close();
});

test("a refresh without opting in asks nothing", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const producer = new TrackProducer("test").accept({});
		const subscriber = producer.subscribe();
		refreshGroup(producer, 0);
		void subscriber.refresh();
		await clock.advance(Duration.secs(3600));
		expect(refreshing(producer)).toBe(false);
		subscriber.close();
	} finally {
		installed.close();
	}
});

// The interval caps how often a one-frame track makes groups: a request asks at once when the
// newest group is old enough, and otherwise waits until it is, then asks once however many
// readers asked meanwhile. A group landing first answers it.
test("an interval defers a refresh until the newest group is old enough", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const interval = Duration.secs(5);
		const producer = new TrackProducer("test").accept({ refresh: { interval } });
		const subscriber = producer.subscribe();
		expect(refreshing(producer)).toBe(true);
		refreshGroup(producer, 0);
		expect(refreshing(producer)).toBe(false);

		await clock.advance(Duration.secs(2));
		void subscriber.refresh();
		const late = producer.subscribe();
		expect(refreshing(producer)).toBe(false);
		await clock.advance(Duration.secs(2));
		expect(refreshing(producer)).toBe(false);
		await clock.advance(Duration.secs(1));
		expect(refreshing(producer)).toBe(true);
		expect(asked(producer)).toBe(1n);
		refreshGroup(producer, 0);
		expect(refreshing(producer)).toBe(false);

		// A request made long after the newest group asks at once.
		await clock.advance(Duration.secs(60));
		void late.refresh();
		expect(refreshing(producer)).toBe(true);
		refreshGroup(producer, 0);

		// A group landing while a request waits answers it.
		void subscriber.refresh();
		refreshGroup(producer, 0);
		await clock.advance(interval);
		expect(refreshing(producer)).toBe(false);

		late.close();
		subscriber.close();
	} finally {
		installed.close();
	}
});

test("a deferred refresh wakes the publisher", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const producer = new TrackProducer("test").accept({ refresh: { interval: Duration.secs(5) } });
		const subscriber = producer.subscribe();
		refreshGroup(producer, 0);
		void subscriber.refresh();

		let woken = 0;
		const dispose = producer.demand().refresh.subscribe((refresh) => {
			if (refresh) woken++;
		});
		await clock.advance(Duration.secs(4));
		expect(woken).toBe(0);
		await clock.advance(Duration.secs(1));
		expect(woken).toBe(1);
		expect(refreshing(producer)).toBe(true);

		dispose();
		subscriber.close();
	} finally {
		installed.close();
	}
});

// With both, a join must outweigh the ratio, while a refresh needs only the interval.
test("a refresh skips the ratio when an interval is declared", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const producer = new TrackProducer("test").accept({ refresh: { ratio: 1, interval: Duration.secs(1) } });
		const subscriber = producer.subscribe();
		refreshGroup(producer, 0);
		await clock.advance(Duration.secs(1));
		const join = producer.subscribe();
		expect(refreshing(producer)).toBe(false);
		void subscriber.refresh();
		expect(refreshing(producer)).toBe(true);

		join.close();
		subscriber.close();
	} finally {
		installed.close();
	}
});

// How a refresh promise settled so far, without waiting on it.
function watch(promise: Promise<void>): { readonly done: boolean; readonly error: unknown } {
	const state: { done: boolean; error: unknown } = { done: false, error: undefined };
	promise.then(
		() => {
			state.done = true;
		},
		(error: unknown) => {
			state.done = true;
			state.error = error;
		},
	);
	return state;
}

// Let settled promises run their reactions.
async function flush(): Promise<void> {
	for (let i = 0; i < 3; i++) await Promise.resolve();
}

// A one-frame track whose refresh interval is 5s, with a subscriber, and a group made at time zero.
function intervalTrack(): { producer: TrackProducer; subscriber: ReturnType<TrackProducer["subscribe"]> } {
	const producer = new TrackProducer("test").accept({ refresh: { interval: Duration.secs(5) } });
	const subscriber = producer.subscribe();
	refreshGroup(producer, 0);
	return { producer, subscriber };
}

// A refresh resolves once it is sent: after the interval, when the publisher reads it. Every
// caller in one interval window shares the one request and resolves with it.
test("concurrent refreshes resolve once the request is sent", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { producer, subscriber } = intervalTrack();
		const first = watch(subscriber.refresh());
		await clock.advance(Duration.secs(2));
		const second = watch(subscriber.refresh());
		const other = producer.subscribe().ordered();
		const third = watch(other.refresh());
		await flush();
		expect([first.done, second.done, third.done]).toEqual([false, false, false]);

		await clock.advance(Duration.secs(3));
		await flush();
		expect(first.done).toBe(false);
		expect(refreshing(producer)).toBe(true);
		await flush();
		expect([first.done, second.done, third.done]).toEqual([true, true, true]);
		const later = watch(subscriber.refresh());
		await flush();
		expect(later.done).toBe(true);
		expect(asked(producer)).toBe(1n);

		other.close();
		subscriber.close();
	} finally {
		installed.close();
	}
});

// A publisher that answers each request as it reads it, within the same batch, still sees the next
// request a caller makes as soon as the first resolves.
test("a refresh made right after one is answered still reaches the publisher", async () => {
	const producer = new TrackProducer("test").accept({ refresh: { interval: Duration.secs(0) } });
	const subscriber = producer.subscribe();
	refreshGroup(producer, 0);
	let answered = 0;
	const dispose = producer.demand().refresh.subscribe((asking) => {
		if (!asking) return;
		answered++;
		refreshGroup(producer, 0);
	});
	try {
		await subscriber.refresh();
		const second = watch(subscriber.refresh());
		for (let i = 0; i < 4 && !second.done; i++) await flush();
		expect(second).toEqual({ done: true, error: undefined });
		expect(answered).toBe(2);
	} finally {
		dispose();
		subscriber.close();
	}
});

// Aborting the last caller's refresh before it is sent withdraws the request, but not one another
// caller still waits on, one a join also made, or one already sent.
test("aborting a refresh withdraws an unsent request", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { producer, subscriber } = intervalTrack();
		const deferred = new AbortController();
		const withdrawn = watch(subscriber.refresh({ signal: deferred.signal }));
		deferred.abort(new Error("gave up"));
		await flush();
		expect(withdrawn.error).toEqual(new Error("gave up"));
		await clock.advance(Duration.secs(5));
		expect(refreshing(producer)).toBe(false);

		// Due, but not yet read.
		const due = new AbortController();
		const dueWithdrawn = watch(subscriber.refresh({ signal: due.signal }));
		due.abort();
		await flush();
		expect(dueWithdrawn.done).toBe(true);
		expect(refreshing(producer)).toBe(false);

		const kept = new AbortController();
		const keptRefresh = watch(subscriber.refresh({ signal: kept.signal }));
		const dropped = new AbortController();
		void subscriber.refresh({ signal: dropped.signal }).catch(() => {});
		dropped.abort();
		expect(refreshing(producer)).toBe(true);
		await flush();
		expect(keptRefresh.done).toBe(true);
		kept.abort();
		expect(refreshing(producer)).toBe(true);
		refreshGroup(producer, 0);

		const join = new AbortController();
		void subscriber.refresh({ signal: join.signal }).catch(() => {});
		const joiner = producer.subscribe();
		join.abort();
		await clock.advance(Duration.secs(5));
		expect(refreshing(producer)).toBe(true);

		const aborted = new AbortController();
		aborted.abort(new Error("before"));
		await expect(subscriber.refresh({ signal: aborted.signal })).rejects.toThrow("before");

		joiner.close();
		subscriber.close();
	} finally {
		installed.close();
	}
});

// A refresh resolves without sending anything when there is nothing to send or a group answers it
// first, and rejects when the track closes first.
test("a refresh ends with the group or the track", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const plain = new TrackProducer("plain").accept({});
		const reader = plain.subscribe();
		refreshGroup(plain, 0);
		await reader.refresh();
		reader.close();

		const { producer, subscriber } = intervalTrack();
		const answered = watch(subscriber.refresh());
		refreshGroup(producer, 0);
		await flush();
		expect(answered).toEqual({ done: true, error: undefined });

		const closed = watch(subscriber.refresh());
		producer.abort(new Error("gone"));
		await flush();
		expect(closed.error).toEqual(new Error("gone"));
		// A refresh asked after the close rejects too rather than waiting forever.
		await expect(subscriber.refresh()).rejects.toThrow("gone");
		const finished = intervalTrack();
		finished.producer.finish();
		finished.producer.close();
		await expect(finished.subscriber.refresh()).rejects.toThrow("closed before the refresh was sent");
		finished.subscriber.close();
		subscriber.close();
	} finally {
		installed.close();
	}
});

test("refresh validates its ratio and rounds its interval up to a millisecond", () => {
	expect(() => authoredInfo({ refresh: { ratio: -1 } })).toThrow(RangeError);
	expect(() => authoredInfo({ refresh: { ratio: 0.5 } })).toThrow(RangeError);
	expect(() => authoredInfo({ refresh: { ratio: 2 ** 32 } })).toThrow(RangeError);
	expect(() => authoredInfo({ refresh: { interval: Duration.millis(Number.MAX_SAFE_INTEGER + 1) } })).toThrow(
		RangeError,
	);
	expect(authoredInfo({ refresh: { ratio: 0 } }).refresh).toEqual({ ratio: 0, interval: null });
	expect(authoredInfo().refresh).toEqual({ ratio: null, interval: null });
	expect(authoredInfo({ refresh: { interval: Duration.micros(1500) } }).refresh.interval?.asMillis()).toBe(2);
});

test("rescale matches the track's timeline, rounded down as Rust's convert does", () => {
	const timed = { timescale: Timescale.MILLI };
	const rescaled = rescale(timed, Timestamp.fromMicros(2_400));
	expect(rescaled?.value).toBe(2);
	expect(rescaled?.scale).toBe(Timescale.MILLI);
	// Halfway and fractional values round down too, where Math.round would give 2 and 3.
	expect(rescale(timed, Timestamp.fromMicros(1_500))?.value).toBe(1);
	expect(rescale(timed, new Timestamp(2.6, Timescale.MILLI))?.value).toBe(2);
	expect(rescale(timed, new Timestamp(1_500.7, Timescale.MICRO))?.value).toBe(1);
	// An aligned value keeps it exactly, where float division gives 4611686020.999999.
	expect(rescale(timed, new Timestamp(4_611_686_021_000_000, Timescale.NANO))?.value).toBe(4_611_686_021);
	// One instant, one result, whatever scale it came from: 2.5/3 s and 5/6 s agree exactly.
	const fine = { timescale: Timescale(Number.MAX_SAFE_INTEGER) };
	const half = rescale(fine, new Timestamp(2.5, Timescale(3)))?.value;
	expect(half).toBe(7_505_999_378_950_825);
	expect(rescale(fine, new Timestamp(5, Timescale(6)))?.value).toBe(half);
	// The same binary value at two scales floors alike, below 0.117 s: 116 ms, as Rust converts it.
	expect(rescale(timed, new Timestamp(0.11699999999999999, Timescale.SECOND))?.value).toBe(116);
	expect(rescale(timed, new Timestamp(131730289100587, Timescale(1125899906842624)))?.value).toBe(116);
	expect(() => rescale(timed, Timestamp.unscaled(2n))).toThrow(TimestampMismatch);
	const huge = new Timestamp(Number.MAX_SAFE_INTEGER, Timescale.MILLI);
	expect(() => rescale({ timescale: Timescale.NANO }, huge)).toThrow(TimestampMismatch);

	const key = Timestamp.unscaled(2n);
	expect(rescale({}, key)).toBe(key);
	expect(rescale({ timescale: null }, key)).toBe(key);
	expect(() => rescale({}, Timestamp.fromMillis(1))).toThrow(TimestampMismatch);
	// A scale-less frame carries its group's key, so another key is a mismatch.
	expect(rescaleIn({ timescale: null }, key, 2n)).toBe(key);
	expect(() => rescaleIn({ timescale: null }, key, 3n)).toThrow(TimestampMismatch);
	expect(rescaleIn(timed, Timestamp.fromMillis(1), 3n).value).toBe(1);
});

// The in-range datagrams, sent between the dropped ones, show the reader is live, so the

// A received compressed datagram inflates far past its wire size, so each reader inflates

// Datagrams and groups are separate channels that share only a sequence namespace, so

test("subscriber options and updates are forwarded to the producer's aggregate", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.ZERO }, ranges: [{ start: { time: 0n } }] });

	// The initial options are available before the request is accepted or put on the wire.
	expect(producer.subscription.peek()).toEqual({
		priority: 0,
		delay: { min: Duration.millis(0), max: Duration.millis(0) },
		order: "descending",
		ranges: [{ start: { time: 0n } }],
	});

	// The wire layer watches the producer's signal to emit SUBSCRIBE_UPDATE.
	const next = producer.subscription.changed();
	track.update({
		priority: 7,
		delay: { max: Duration.millis(250) },
		order: "ascending",
		ranges: [{ start: { time: 2n }, end: { time: 8n } }],
	});
	expect(await next).toEqual({
		priority: 7,
		delay: { min: Duration.millis(0), max: Duration.millis(250) },
		order: "ascending",
		ranges: [{ start: { time: 2n }, end: { time: 8n } }],
	});
});

test("subscriber maxAge and maxDelay are refused before defaults or updates take effect", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(250) } });
	const original = producer.subscription.peek();

	for (const maxAge of [Duration.millis(500), undefined]) {
		expect(() => producer.subscribe({ maxAge } as Subscription)).toThrow("delay.max");
		const options = { maxDelay: Duration.millis(100), maxAge } as Subscription;
		expect(() => producer.subscribe(options)).toThrow(TypeError);
		expect(() => track.update(options)).toThrow("delay.max");
		expect(() => producer.subscribe({ maxDelay: Duration.millis(100) } as Subscription)).toThrow("delay.max");
		expect(producer.subscription.peek()).toEqual(original);
	}

	// The publisher's own staleness bound keeps its distinct maxAge field.
	producer.accept({ maxAge: Duration.millis(500) });
	track.close();
	producer.finish();
	producer.close();
});

// A typo from plain JS would otherwise be served as the default without a word.
test("an unknown subscriber order is refused", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.subscribe({ order: "newest" } as unknown as Subscription)).toThrow(TypeError);
	expect(producer.subscription.peek()).toBeUndefined();
	producer.finish();
	producer.close();
});

test("a fractional delay is rounded up before the wire sees it", async () => {
	const producer = new TrackProducer("test");

	// Subscribers derive this from measurements (a jitter estimate scaled off RTT), so a
	// fractional millisecond is expected. The wire encodes it as a varint, which throws on a
	// non-integer, and rounding down would shorten a budget the subscriber asked for.
	const track = producer.subscribe({ delay: { max: Duration.millis(38.75) } });
	expect(producer.subscription.peek()?.delay?.max).toEqual(Duration.millis(39));

	const next = producer.subscription.changed();
	track.update({ delay: { max: Duration.millis(500.25) } });
	expect((await next)?.delay?.max).toEqual(Duration.millis(501));
	const min = producer.subscription.changed();
	track.update({ delay: { min: Duration.millis(99.5) } });
	expect((await min)?.delay?.min).toEqual(Duration.millis(100));

	// The publisher half of the budget lands on the wire through TRACK_INFO, with the same hazard.
	producer.accept({ maxAge: Duration.millis(1000.5) });
	expect((await producer.info()).maxAge).toEqual(Duration.millis(1001));
});

test("a delay or maxAge past the wire's millisecond range is refused", () => {
	const producer = new TrackProducer("test");

	expect(() => producer.accept({ maxAge: Duration.millis(Number.MAX_SAFE_INTEGER + 1) })).toThrow(RangeError);
	expect(() => producer.subscribe({ delay: { max: Duration.millis(Number.MAX_SAFE_INTEGER + 1) } })).toThrow(
		RangeError,
	);
});

test("priority -128..=127 is accepted and anything outside is refused", () => {
	const producer = new TrackProducer("video");
	producer.accept({ priority: 127 });
	expect(producer.priority).toBe(127);
	expect(new TrackProducer("audio").subscribe({ priority: -128 }).subscription.peek()?.priority).toBe(-128);
	expect(() => producer.accept({ priority: 128 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: -129 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: 1.5 })).toThrow(RangeError);
	expect(() => producer.subscribe({ priority: 128 })).toThrow(RangeError);
});

test("authoredInfo re-validates timescale and the safe maxAge bound", () => {
	expect(authoredInfo({ timescale: Timescale.MICRO }).timescale).toBe(Timescale.MICRO);
	expect(authoredInfo({ maxAge: Duration.millis(Number.MAX_SAFE_INTEGER) }).maxAge).toEqual(
		Duration.millis(Number.MAX_SAFE_INTEGER),
	);
	expect(() => authoredInfo({ timescale: 0 as Timescale })).toThrow(RangeError);
	expect(() => authoredInfo({ timescale: (Number.MAX_SAFE_INTEGER + 1) as Timescale })).toThrow(RangeError);
	expect(() => authoredInfo({ maxAge: Duration.millis(Number.MAX_SAFE_INTEGER + 1) })).toThrow(RangeError);
});

test("an authored track defaults to a zero max age, and null is unlimited", async () => {
	expect((await new TrackProducer("omitted").accept().info()).maxAge).toEqual(Duration.ZERO);
	expect((await new TrackProducer("undefined").accept({ maxAge: undefined }).info()).maxAge).toEqual(Duration.ZERO);
	expect((await new TrackProducer("unlimited").accept({ maxAge: null }).info()).maxAge).toBeNull();
	expect((await new TrackProducer("explicit").accept({ maxAge: Duration.millis(5000) }).info()).maxAge).toEqual(
		Duration.millis(5000),
	);
});

// The catalog relies on this: a track that publishes one snapshot and never changes it stays
// readable to a viewer joining long after, despite the zero default max age, since only a
// superseded group goes stale and the live edge is exempt from the payload cache window.
test("a sole snapshot stays readable to a late joiner at the zero default max age", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("catalog").accept({ timescale: Timescale.MILLI, compression: "deflate" });
	try {
		expect((await producer.info()).maxAge).toEqual(Duration.ZERO);
		const group = hooks.appendNext(producer);
		group.writeFrame(textFrame("snapshot", Timestamp.fromMillis(0)));
		group.finish();

		// Well past the payload cache window.
		await clock.advance(10_000 + 10 * CACHE_WINDOW_MS);
		for (const delay of [undefined, { max: null }] as const) {
			const late = producer.subscribe(delay ? { delay } : undefined);
			const joined = late.tryRecvGroup();
			if (!joined) throw new Error("the sole snapshot was not served");
			expect(joined.start).toBe(0n);
			expect(await joined.readString()).toBe("snapshot");
			late.close();
		}

		// A declared end keeps the track open, so its former edge is still exempt.
		producer.finish();
		await clock.advance(10_000 + 20 * CACHE_WINDOW_MS);
		const late = producer.subscribe();
		const joined = late.tryRecvGroup();
		if (!joined) throw new Error("the finished snapshot was not served");
		expect(await joined.readString()).toBe("snapshot");
		late.close();
	} finally {
		producer.close();
		clock.installed.close();
	}
});

test("multiple subscriber options aggregate like Rust", async () => {
	const producer = new TrackProducer("test");
	const bounded = producer.subscribe({
		priority: 2,
		delay: { min: Duration.millis(400), max: Duration.millis(100) },
		order: "ascending",
		ranges: [{ start: { time: 10n }, end: { time: 19n } }],
	});
	const live = producer.subscribe({
		priority: 7,
		delay: { min: Duration.millis(50), max: Duration.millis(250) },
		ranges: [{ start: { time: 5n } }],
	});
	// A minimum that went unstated, as a legacy peer's does, counts for nothing.
	const legacy = producer.subscribe({
		delay: { min: null, max: Duration.millis(0) },
		ranges: [{ start: { time: 5n } }],
	});

	// One descending subscriber makes the aggregate descending, so a live reader is never held
	// behind another's backlog. The largest minimum wins, so an upstream reaches back far enough
	// for the reader that buffers most.
	expect(producer.subscription.peek()).toEqual({
		priority: 7,
		delay: { min: Duration.millis(400), max: Duration.millis(250) },
		order: "descending",
		ranges: [{ start: { time: 5n } }],
	});

	legacy.close();
	const narrowed = producer.subscription.changed();
	live.close();
	expect(await narrowed).toEqual({
		priority: 2,
		delay: { min: Duration.millis(400), max: Duration.millis(100) },
		order: "ascending",
		ranges: [{ start: { time: 10n }, end: { time: 19n } }],
	});

	const none = producer.subscription.changed();
	bounded.close();
	expect(await none).toBeUndefined();
});

// A reader without ranges reads from the oldest group, as Rust's `Range::oldest()` default does,
// so beside a history reader the union still reaches the history floor and below it. A reader that
// asks for the live edge alone keeps the history reader's floor as its own range.
test("a reader without ranges keeps a history reader's floor", () => {
	const producer = new TrackProducer("test");
	const history = producer.subscribe({ ranges: [{ start: { time: 5n }, end: { time: 8n } }] });
	const plain = producer.subscribe();
	expect(producer.subscription.peek()?.ranges).toEqual([{}]);
	expect(rangesStart(producer.subscription.peek()?.ranges ?? [])).toBeUndefined();

	plain.close();
	history.close();

	const other = new TrackProducer("test");
	const kept = other.subscribe({ ranges: [{ start: { time: 5n }, end: { time: 8n } }] });
	const live = other.subscribe({ ranges: [LIVE] });
	expect(other.subscription.peek()?.ranges).toEqual([
		{ start: { relative: 0n } },
		{ start: { time: 5n }, end: { time: 8n } },
	]);
	live.close();
	kept.close();
});

test("each reader reads only its own ranges, whatever the union asks for", () => {
	const producer = new TrackProducer("test");
	const sparse = producer.subscribe({
		delay: { max: null },
		ranges: [
			{ start: { time: 1n }, end: { time: 2n } },
			{ start: { time: 5n }, end: { time: 5n } },
		],
	});
	const tail = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 4n } }] });
	expect(producer.subscription.peek()?.ranges).toEqual([
		{ start: { time: 1n }, end: { time: 2n } },
		{ start: { time: 4n } },
	]);
	for (let key = 0n; key < 7n; key++) producer.createGroup(key).finish();

	const read = (subscriber: typeof sparse) => {
		const got: bigint[] = [];
		for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup()) got.push(group.start);
		return got;
	};
	expect(read(sparse)).toEqual([1n, 2n, 5n]);
	expect(read(tail)).toEqual([4n, 5n, 6n]);

	// Replacing the ranges offers the held groups the new ones take in, never one read already.
	sparse.update({ delay: { max: null }, ranges: [{ start: { time: 0n }, end: { time: 4n } }] });
	expect(read(sparse)).toEqual([0n, 3n, 4n]);
	producer.finish();
	expect(hooks.tryRecvGroup(sparse, false)).toEqual({ kind: "done" });
});

// Mirrors the Rust `a_bounded_read_stays_ended_until_widened` and its siblings: a read whose
// ranges all end finishes once nothing in them is left, though the track goes on.
test("a bounded read ends once its window is read, unless a cap or a wire publisher holds it", () => {
	const producer = new TrackProducer("test");
	const ranges = [{ start: { time: 1n }, end: { time: 4n } }];
	const read = (subscriber: ReturnType<typeof producer.subscribe>) => {
		const got: bigint[] = [];
		for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup()) got.push(group.start);
		return got;
	};
	const reader = producer.subscribe({ delay: { max: null }, ranges });
	const sequenced = producer.subscribe({ delay: { max: null }, ranges });
	const ordered = sequenced.ordered();
	const capped = producer.subscribe({ delay: { max: null }, ranges }).withRange({ end: { excluded: 3n } });
	const parked = hooks.parked(producer.subscribe({ delay: { max: null }, ranges }));
	const open = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1n } }] });
	for (const key of [0n, 1n, 2n]) producer.createGroup(key).finish();
	expect(read(reader)).toEqual([1n, 2n]);
	// Groups 3 and 4 may still arrive.
	expect(hooks.tryRecvGroup(reader, false)).toEqual({ kind: "idle" });

	hooks.dropTrackGroups(producer, 3n, 4n, { error: DROPPED });
	producer.createGroup(4n).finish();
	expect(read(reader)).toEqual([4n]);
	expect(hooks.tryRecvGroup(reader, false)).toEqual({ kind: "done" });
	expect(drainOrdered(ordered)).toEqual([1n, 2n, 4n]);
	expect(hooks.tryNextGroup(sequenced)).toEqual({ kind: "done" });
	// A wire publisher's peer would read the end of the stream as the track's.
	expect(read(parked)).toEqual([1n, 2n, 4n]);
	expect(hooks.tryRecvGroup(parked, false)).toEqual({ kind: "idle" });
	expect(read(open)).toEqual([1n, 2n, 4n]);
	expect(hooks.tryRecvGroup(open, false)).toEqual({ kind: "idle" });
	// The cap keeps group 4 out, and may rise.
	expect(read(capped)).toEqual([1n, 2n]);
	expect(hooks.tryRecvGroup(capped, false).kind).not.toBe("done");
	capped.withRange({});
	expect(read(capped)).toEqual([4n]);
	expect(hooks.tryRecvGroup(capped, false)).toEqual({ kind: "done" });

	// Replacing the ranges reads on.
	reader.update({ delay: { max: null }, ranges: [{ start: { time: 0n }, end: { time: 5n } }] });
	expect(read(reader)).toEqual([0n]);
	expect(hooks.tryRecvGroup(reader, false)).toEqual({ kind: "idle" });
	producer.createGroup(5n).finish();
	expect(read(reader)).toEqual([5n]);
	expect(hooks.tryRecvGroup(reader, false)).toEqual({ kind: "done" });
});

// A subscribed copy owes a hole until its feed gives up on it, as Rust's bounded wait does.
test("a bounded read on a subscribed copy ends once its feed stops owing a hole", async () => {
	const clock = manualTime(0);
	try {
		const producer = receivedTrack();
		const reader = producer.subscribe({
			delay: { max: Duration.millis(1000) },
			ranges: [{ end: { time: 2n } }],
		});
		putGroup(producer, 0n);
		putGroup(producer, 2n);
		expect(reader.tryRecvGroup()?.start).toBe(0n);
		expect(reader.tryRecvGroup()?.start).toBe(2n);
		const ended = reader.recvGroup();
		await clock.advance(999);
		expect(await Promise.race([ended, Promise.resolve("waiting")])).toBe("waiting");
		await clock.advance(1000);
		expect(await ended).toBeUndefined();
	} finally {
		clock.installed.close();
	}
});

test("an ordered reader keeps a group between its ranges until the cursor passes it", () => {
	const producer = new TrackProducer("test");
	// Time anchors name the group starting there, held or not, so group 1 falls between the ranges.
	hooks.startsAtKeys(producer);
	const track = producer
		.subscribe({
			delay: { max: null },
			ranges: [
				{ start: { time: 0n }, end: { time: 0n } },
				{ start: { time: 3n }, end: { time: 3n } },
			],
		})
		.ordered();
	for (const key of [0n, 1n]) producer.createGroup(key).finish();
	expect(track.tryNextGroup()?.start).toBe(0n);
	expect(track.tryNextGroup()).toBeUndefined();

	track.update({ delay: { max: null }, ranges: [{ start: { time: 0n }, end: { time: 3n } }] });
	expect(track.tryNextGroup()?.start).toBe(1n);
	track.close();
	producer.finish();
	producer.close();
});

test("a relative range counts back from the latest group, or waits for the first", () => {
	const producer = new TrackProducer("test");
	for (let key = 0n; key < 5n; key++) producer.createGroup(key).finish();
	const back = producer.subscribe({
		delay: { max: null },
		ranges: [{ start: { relative: 2n }, end: { relative: 1n } }],
	});
	expect([back.tryRecvGroup()?.start, back.tryRecvGroup()?.start, back.tryRecvGroup()]).toEqual([2n, 3n, undefined]);

	const empty = new TrackProducer("test");
	const live = empty.subscribe({ delay: { max: null }, ranges: [{ start: { relative: 0n } }] });
	empty.createGroup(7n).finish();
	empty.createGroup(6n).finish();
	empty.createGroup(8n).finish();
	// Resolved at the first group to arrive: group 6 sits below where it started.
	expect([live.tryRecvGroup()?.start, live.tryRecvGroup()?.start, live.tryRecvGroup()]).toEqual([7n, 8n, undefined]);
});

test("a copy over a wire without ranges refuses a relative anchor it cannot carry", () => {
	const producer = new TrackProducer("test");
	const back = producer.subscribe({ ranges: [{ start: { relative: 3n } }] });
	const live = producer.subscribe({ ranges: [{ start: { relative: 0n } }] });
	hooks.dropAnchors(producer);
	producer.createGroup(0n).finish();
	expect(() => back.tryRecvGroup()).toThrow("moq-lite-07");
	expect(live.tryRecvGroup()?.start).toBe(0n);
});

test("a time range reads the groups containing its times, by their starts", () => {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	const start = (key: bigint) => {
		const group = producer.createGroup(key);
		group.writeFrame(textFrame(`${key}`, Timestamp.fromMillis(Number(key))));
		return group;
	};
	for (const key of [0n, 1000n, 2000n]) start(key).finish();
	const reader = producer.subscribe({
		delay: { max: null },
		ranges: [{ start: { time: 1500n }, end: { time: 3500n } }],
	});
	const read = () => {
		const got: bigint[] = [];
		for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) got.push(group.start);
		return got;
	};
	// Group 1000 contains 1500, and a group may still start inside the end.
	expect(read()).toEqual([1000n, 2000n]);
	// A group's key is its start, so the end places it before any frame arrives.
	producer.createGroup(3000n);
	expect(read()).toEqual([3000n]);
	producer.createGroup(4000n);
	expect(read()).toEqual([]);
	expect(hooks.servingEnd(reader)).toBe(3501n);
});

// Every group `reader` can take now, in delivery order.
function readable(reader: ReturnType<TrackProducer["subscribe"]>): bigint[] {
	const got: bigint[] = [];
	for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) got.push(group.start);
	return got;
}

// A timed track of 500 ms groups starting at 0 through 5 s, each presenting at its start.
function halfSecondGroups(): TrackProducer {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	for (let n = 0n; n <= 10n; n++) appendHalfSecond(producer, n);
	return producer;
}

// Append the `n`th 500 ms group, starting and presenting at `n` half seconds.
function appendHalfSecond(producer: TrackProducer, n: bigint): void {
	const group = producer.createGroup(n * 500n);
	group.writeFrame(textFrame(`${n}`, Timestamp.fromMillis(Number(n) * 500)));
	group.finish();
}

// The starts of the 500 ms groups `from` through `to`, in milliseconds.
function halfSeconds(from: number, to: number): bigint[] {
	return Array.from({ length: to - from + 1 }, (_, n) => BigInt((from + n) * 500));
}

test("a live join reaches back by its min age", () => {
	const producer = halfSecondGroups();
	const reader = producer.subscribe({ delay: { min: Duration.secs(2), max: Duration.secs(30) }, ranges: [LIVE] });
	// The newest frame presents at 5 s, so the join starts at the group containing 3 s.
	expect(readable(reader)).toEqual(halfSeconds(6, 10));
	appendHalfSecond(producer, 11n);
	expect(readable(reader)).toEqual([5500n]);
});

test("a min age update reaches a live join not yet started", () => {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	const reader = producer.subscribe({ delay: { min: Duration.secs(1), max: Duration.secs(30) }, ranges: [LIVE] });
	expect(readable(reader)).toEqual([]);
	reader.update({ delay: { min: Duration.secs(2), max: Duration.secs(30) }, ranges: [LIVE] });
	for (let n = 0n; n <= 10n; n++) appendHalfSecond(producer, n);
	expect(readable(reader)).toEqual(halfSeconds(6, 10));
});

test("a live join without a min age starts at the latest group, and the oldest start ignores it", () => {
	const producer = halfSecondGroups();
	const all = halfSeconds(0, 10);
	for (const min of [null, Duration.ZERO]) {
		const live = producer.subscribe({ delay: { min, max: Duration.secs(30) }, ranges: [LIVE] });
		expect(readable(live)).toEqual([5000n]);
	}
	for (const min of [null, Duration.ZERO, Duration.secs(2)]) {
		const oldest = producer.subscribe({ delay: { min, max: Duration.secs(30) } });
		expect(readable(oldest)).toEqual(all);
	}
	// Past everything held, it starts at the oldest group; a named start ignores it.
	const far = producer.subscribe({ delay: { min: Duration.secs(60), max: Duration.secs(30) }, ranges: [LIVE] });
	expect(readable(far)).toEqual(all);
	const named = producer.subscribe({
		delay: { min: Duration.secs(2), max: Duration.secs(30) },
		ranges: [{ start: { relative: 1n } }],
	});
	expect(readable(named)).toEqual([4500n, 5000n]);
});

test("a live join reads nothing until the groups held measure the live edge", () => {
	const producer = new TrackProducer("test");
	// A copy whose wire keys groups by sequence measures by first frames, not keys.
	hooks.startsAtKeys(producer);
	// Groups written before accept say nothing yet of whether the track is timed.
	const groups = Array.from({ length: 11 }, (_, key) => producer.createGroup(BigInt(key)));
	const reader = producer.subscribe({ delay: { min: Duration.secs(2), max: Duration.secs(30) }, ranges: [LIVE] });
	expect(readable(reader)).toEqual([]);
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	// Accepted, but still only headers: nothing to measure by.
	expect(readable(reader)).toEqual([]);
	for (const [key, group] of groups.entries()) {
		group.writeFrame(textFrame(`${key}`, Timestamp.fromMillis(key * 500)));
		group.finish();
	}
	expect(readable(reader)).toEqual([6n, 7n, 8n, 9n, 10n]);
});

test("a live join reaches back by ticks rounded up, at any timestamp the track accepts", () => {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	// Past what microseconds can hold as a safe number.
	const base = 10_000_000_000_000;
	const starts = [0, 1, 2].map((offset) => BigInt(base + offset));
	for (const start of starts) {
		const group = producer.createGroup(start);
		group.writeFrame(textFrame(`${start}`, Timestamp.fromMillis(Number(start))));
		group.finish();
	}
	// 1.5 ms back from 2 ms past the base is 0.5 ms past it, which only the first group starts by.
	const reader = producer.subscribe({
		delay: { min: Duration.micros(1500), max: Duration.secs(30) },
		ranges: [LIVE],
	});
	expect(readable(reader)).toEqual(starts);
});

test("a live join measures timestamps between ticks, floored", () => {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI, maxAge: null });
	for (const [key, ms] of [0.5, 1.5, 2.5].entries()) {
		const group = producer.createGroup(BigInt(key));
		group.writeFrame(textFrame(`${key}`, Timestamp.fromMillis(ms)));
		group.finish();
	}
	// The edge floors to 2 ms, so 1 ms back is the group starting by 1 ms.
	const reader = producer.subscribe({ delay: { min: Duration.millis(1), max: Duration.secs(30) }, ranges: [LIVE] });
	expect(readable(reader)).toEqual([1n, 2n]);
});

test("a scale-less join reaches back by arrival", () => {
	const clock = manualTime(0);
	try {
		const producer = new TrackProducer("test");
		producer.accept({ maxAge: null });
		// Group N arrives N seconds in, and the join comes at 5 s.
		for (let key = 0n; key < 5n; key++) {
			const group = producer.createGroup(key);
			group.writeFrame(textFrame(`${key}`, Timestamp.unscaled(key)));
			group.finish();
			clock.set(Number(key + 1n) * 1000);
		}
		const reader = producer.subscribe({
			delay: { min: Duration.millis(2500), max: Duration.secs(30) },
			ranges: [LIVE],
		});
		expect(readable(reader)).toEqual([2n, 3n, 4n]);
		producer.close();
	} finally {
		clock.installed.close();
	}
});

// A time start resolves on keys alone: groups opened before their first frames, below a
// reached end, are read from the one containing it. Mirrors the Rust `a_time_start_needs_no_frame`.
test("a time start resolves on keys alone, before any group's first frame", () => {
	for (const order of ["ascending", "descending"] as const) {
		const producer = new TrackProducer("test");
		producer.accept({ timescale: Timescale.MILLI, maxAge: null });
		for (const key of [0n, 1000n, 2000n]) producer.createGroup(key);
		producer.finishAt({ group: 3000n, frame: 0 });
		const subscription = { delay: { max: null }, order, ranges: [{ start: { time: 1500n } }] };
		const reader = producer.subscribe(subscription);
		const ordered = producer.subscribe(subscription).ordered();
		const got: bigint[] = [];
		for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) got.push(group.start);
		expect(got.sort()).toEqual([1000n, 2000n]);
		const inOrder: bigint[] = [];
		for (let group = ordered.tryNextGroup(); group; group = ordered.tryNextGroup()) inOrder.push(group.start);
		expect(inOrder).toEqual([1000n, 2000n]);
	}
});

// A drop inside a time end is reported, and one past it is not: the end settles by key.
test("a drop a time end covers is reported", () => {
	const producer = new TrackProducer("test");
	producer.accept({ timescale: Timescale.MILLI });
	const zero = producer.createGroup(0n);
	const reader = producer.subscribe({
		delay: { max: null },
		order: "ascending",
		ranges: [{ start: { time: 0n }, end: { time: 500n } }],
	});
	expect(hooks.tryRecvGroup(reader, true)).toMatchObject({ kind: "group", group: { start: 0n } });
	hooks.dropTrackGroups(producer, 600n, 700n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });
	zero.writeFrame(textFrame("0", Timestamp.fromMillis(0)));
	hooks.dropTrackGroups(producer, 100n, 200n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 100n, end: 200n, error: DROPPED });
});

// A scale-less track's keys are bare units, so a time anchor reads them as they are. Mirrors the
// Rust `an_untimed_track_reads_time_anchors_as_keys`.
test("an untimed track reads time anchors as keys", () => {
	const untimed = new TrackProducer("test");
	untimed.accept({ maxAge: null });
	for (const key of [0n, 5n, 10n, 15n]) untimed.appendGroup(Timestamp.unscaled(key)).finish();
	const reader = untimed.subscribe({ delay: { max: null }, ranges: [{ start: { time: 7n }, end: { time: 10n } }] });
	expect(readable(reader)).toEqual([5n, 10n]);
});

test("only unstated minimums aggregate to an unstated one", () => {
	const producer = new TrackProducer("test");
	producer.subscribe({ delay: { min: null } });
	producer.subscribe({ delay: { min: null, max: Duration.millis(250) } });
	expect(producer.subscription.peek()?.delay).toEqual({ min: null, max: Duration.millis(250) });
});

// A resume floor above the live edge must not hide the latest group from a reader that
// joins after it: on a quiet track that group may be the only one for a long time.
test("an unfloored subscriber clears a resume floor above the live edge", () => {
	const producer = new TrackProducer("test");
	producer.subscribe({ ranges: [{ start: { time: 4n } }] });
	producer.subscribe({});
	expect(producer.subscription.peek()?.ranges).toEqual([{}]);
});

test("the producer aggregate is not clamped to the track's maxAge", () => {
	// The publisher applies its own, so a subscriber asking upstream before the info lands
	// asks for the same demand after it.
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(10_000) } });
	const before = producer.subscription.peek();

	producer.accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(2_000) });

	expect(producer.subscription.peek()).toEqual(before);
	expect(producer.subscription.peek()?.delay?.max).toEqual(Duration.millis(10_000));
	expect(track.subscription.peek()?.delay?.max).toEqual(Duration.millis(10_000));
	// A late group is waited for no longer than the publisher keeps it fresh.
	expect(hooks.tailGrace(producer)).toEqual(Duration.millis(2_000));
});

test("a null delay.max dominates when subscriptions combine", () => {
	const producer = new TrackProducer("test");
	producer.subscribe({ delay: { max: null } });
	producer.subscribe({ delay: { max: Duration.millis(250) } });
	// The most tolerant subscriber wins the aggregate, so no limit dominates.
	expect(producer.subscription.peek()?.delay?.max).toEqual(Duration.millis(Number.MAX_SAFE_INTEGER));
});

test("nextGroup skips late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();

	producer.createGroup(5n);

	const first = await track.nextGroup();
	expect(first?.start).toBe(5n);

	// Late arrivals with sequence <= last returned are skipped.
	producer.createGroup(3n);
	producer.createGroup(4n);
	producer.createGroup(7n);

	const next = await track.nextGroup();
	expect(next?.start).toBe(7n);
});

test("nextGroup returns buffered groups in sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();

	producer.createGroup(3n);
	producer.createGroup(5n);

	expect((await track.nextGroup())?.start).toBe(3n);
	expect((await track.nextGroup())?.start).toBe(5n);
});

test.each([false, true])("a reordered tail keeps the group PTS drift verdict (B-frame: %s)", async (reordered) => {
	const clock = manualTime(0);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(80) } });
		for (const ms of [0, 100]) {
			const group = hooks.appendNext(producer);
			group.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(ms) });
			group.finish();
		}
		const edge = hooks.appendNext(producer);
		for (const ms of [150, 200])
			edge.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(ms) });
		if (reordered) edge.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(160) });
		edge.finish();
		// Group 0 reaches 100ms and is 100ms behind the highest PTS; group 1 reaches 150ms.
		expect((await subscriber.recvGroup())?.start).toBe(1n);
		subscriber.close();
		producer.finish();
		producer.close();
	} finally {
		clock.installed.close();
	}
});

// A group's age is where its content *ends*, not where it began. A long group whose tail
// is level with the live edge still owes the reader every frame in it, so judging it by
// its first timestamp would discard exactly the group being filled.
test("a long group is not stale while its tail reaches the edge", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const sub = producer.subscribe({ delay: { max: Duration.millis(500) } });

	// Group 0 spans 0..2000ms; group 2000 starts there, where group 0 ends.
	const long = hooks.appendNext(producer);
	for (const ms of [0, 500, 1000, 1500, 2000]) {
		long.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	}
	long.finish();
	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });

	expect((await sub.recvGroup())?.start).toBe(0n);
	expect((await sub.recvGroup())?.start).toBe(2000n);
});

test("a long group is stale once its successor falls behind", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const sub = producer.subscribe({ delay: { max: Duration.millis(500) } });

	const long = hooks.appendNext(producer);
	for (const ms of [0, 500, 1000]) {
		long.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	}
	long.finish();
	// Group 0 reaches at most 3s (where group 3000 starts), a second behind the 4s edge and
	// so past the budget. A frame's duration is not on the wire, so group 0's own last
	// timestamp proves nothing about where it ends; only group 3000's start bounds it.
	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(3000) });
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(4000) });

	expect((await sub.recvGroup())?.start).toBe(3000n);
	expect((await sub.recvGroup())?.start).toBe(4000n);
});

// Datagrams are unordered by construction, so the sequence cursor carries them too: a

// Both cursors write a backlog off: a buffered group is not free to deliver, since a
// consumer reading one that is already too old plays it at 1x and never catches up.
test("the latency budget skips a buffered timeline on either cursor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const arrival = producer.subscribe();
	const ordered = producer.subscribe().ordered();

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await arrival.recvGroup())?.start).toBe(2000n);
	expect((await ordered.nextGroup())?.start).toBe(2000n);

	// The budget is the only gate: one that spans the backlog bursts it in order.
	const replay = producer.subscribe({ delay: { max: Duration.millis(30_000) } }).ordered();
	expect((await replay.nextGroup())?.start).toBe(0n);
	expect((await replay.nextGroup())?.start).toBe(1000n);
	expect((await replay.nextGroup())?.start).toBe(2000n);
});

// A frameless successor is not content, so the next started group bounds the group before
// it. That bound is sound: the frameless group's first frame is checked to start at or below
// the next group's, so the true reach can only be earlier.
test("a frameless successor leaves the next started group as the bound", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	// Any budget, so only media time judges: the wall clock barely moves here.
	const track = producer.subscribe({ delay: { max: Duration.millis(500) } });

	producer.writeFrame({ payload: enc.encode("0"), timestamp: Timestamp.fromMillis(0) });
	hooks.appendNext(producer); // group 1 stalls before its first frame
	producer.writeFrame({ payload: enc.encode("2"), timestamp: Timestamp.fromMillis(10_000) });
	producer.writeFrame({ payload: enc.encode("3"), timestamp: Timestamp.fromMillis(11_000) });

	// Groups 0 and 1 both reach at most group 10000's start, a second behind the edge: stale.
	expect(drain(track)).toEqual([10_000n, 11_000n]);
});

// Groups can arrive out of sequence order; the immediate successor bounds reach even when it
// arrived last.
test("a late-arriving successor bounds a group's reach", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe({ delay: { max: Duration.millis(500) } });

	for (const [key, ms] of [
		[0n, 0],
		[2n, 3000],
		[1n, 200],
	] as const) {
		const group = producer.createGroup(key);
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
		group.finish();
	}

	// Group 0 reaches at most 200ms, where group 1 starts, so it is past the budget.
	// Group 1 reaches 3000ms, the edge itself.
	expect((await track.recvGroup())?.start).toBe(1n);
	expect((await track.recvGroup())?.start).toBe(2n);
});

// The ordered frame helpers ride the same cursor, so they see the same budget: a
// backlog inside it is drained in full, and what is past it is skipped.
test("ordered frame reads follow the budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const ordered = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();
	const live = producer.subscribe().ordered();

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect(await ordered.readString()).toBe("0");
	expect(await ordered.readString()).toBe("1000");
	expect(await ordered.readString()).toBe("2000");

	expect(await live.readString()).toBe("2000");
});

// Interleaving a group take with the frame helpers is still one cursor: a take that
// moves past the group the frame helpers were draining abandons it, so a later frame
// read never runs backwards.
for (const take of ["nextGroup", "tryNextGroup"] as const) {
	test(`${take} abandons the frame helpers' group when it passes it`, async () => {
		const producer = new TrackProducer("test");
		const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();

		const zero = producer.createGroup(0n);
		zero.writeFrame(textFrame("0.0", Timestamp.fromMillis(Number(zero.start))));
		zero.writeFrame(textFrame("0.1", Timestamp.fromMillis(Number(zero.start))));
		zero.finish();

		const one = producer.createGroup(1n);
		one.writeFrame(textFrame("1.0", Timestamp.fromMillis(Number(one.start))));
		one.finish();

		const two = producer.createGroup(2n);
		two.writeFrame(textFrame("2.0", Timestamp.fromMillis(Number(two.start))));
		two.finish();

		// The frame path enters group 0; a direct take then claims group 1.
		expect((await track.readFrame())?.group).toBe(0n);
		expect((await track[take]())?.start).toBe(1n);

		// The frame path must not resume group 0 behind the cursor: it continues at 2.
		const next = await track.readFrame();
		expect(next?.group).toBe(2n);
		expect(dec.decode(next?.payload)).toBe("2.0");
	});
}

// The frame helpers ride the same sequence cursor as nextGroup: a lower group arriving
// after a higher one was read is skipped, never fed to the caller backwards.
test("ordered frame reads skip a late lower-sequence group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();

	const five = producer.createGroup(5n);
	five.writeFrame(textFrame("five", Timestamp.fromMillis(5)));
	five.finish();
	expect(await track.readString()).toBe("five");

	const three = producer.createGroup(3n);
	three.writeFrame(textFrame("three", Timestamp.fromMillis(3)));
	three.finish();

	const six = producer.createGroup(6n);
	six.writeFrame(textFrame("six", Timestamp.fromMillis(6)));
	six.finish();

	expect(await track.readString()).toBe("six");
});

test("zero latency takes the latest group when ages are equal", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(5000),
		});
		const track = producer.subscribe();
		appendFrozen(producer, "old");
		appendFrozen(producer, "new");

		expect((await track.recvGroup())?.start).toBe(1n);
	} finally {
		clock.installed.close();
	}
});

test("an omitted delay is the default zero, and a null max sets no limit", async () => {
	expect(Subscription.default()).toEqual({
		priority: 0,
		delay: { min: Duration.ZERO, max: Duration.ZERO },
		order: "descending",
	});

	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const explicit = producer.subscribe(Subscription.default());
	const implicit = producer.subscribe();
	const bare = producer.subscribe({});
	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Each spelling of the default keeps only the newest group.
	expect((await explicit.recvGroup())?.start).toBe(2000n);
	expect((await implicit.recvGroup())?.start).toBe(2000n);
	expect((await bare.recvGroup())?.start).toBe(2000n);
	expect(producer.subscription.peek()?.delay?.max).toEqual(Duration.ZERO);

	// An explicit null leaves the publisher's max age as the only bound.
	const unlimited = producer.subscribe({ delay: { max: null } });
	for (const key of [0n, 1000n, 2000n]) expect((await unlimited.recvGroup())?.start).toBe(key);
	expect(producer.subscription.peek()?.delay?.max).toEqual(Duration.millis(Number.MAX_SAFE_INTEGER));
	expect(hooks.tailGrace(producer)).toEqual(Duration.millis(5000));
	producer.finish();
	producer.close();
});

test("latency budget admits groups within its presentation-time window", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe({ delay: { max: Duration.millis(1500) } });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Group 0 reaches 1s, only 1s behind the 2s edge, so a 1.5s budget still admits it.
	expect((await track.recvGroup())?.start).toBe(0n);
	expect((await track.recvGroup())?.start).toBe(1000n);
	expect((await track.recvGroup())?.start).toBe(2000n);
});

test("a late lower group within the budget is delivered", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(30_000) });
	for (const [key, timestamp] of [
		[5n, 1000],
		[6n, 2000],
		[7n, 3000],
	] as const) {
		const group = producer.createGroup(key);
		group.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
		group.finish();
	}

	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	expect((await track.recvGroup())?.start).toBe(5n);
	expect((await track.recvGroup())?.start).toBe(6n);
	expect((await track.recvGroup())?.start).toBe(7n);

	// Arriving below everything already delivered is not what makes content stale: the
	// budget is the only gate, and this straggler's timestamp is within it. A consumer
	// that needs sequence order reorders (or drops) it itself.
	const late = producer.createGroup(4n);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(500) });
	late.finish();

	expect((await track.recvGroup())?.start).toBe(4n);
	producer.finish();
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
});

test("a named start is a floor, not a request", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(30_000) });
	for (const timestamp of [0, 1000, 2000, 3000, 4000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// The budget is the only thing that asks for data; a named start only bounds how far
	// back it may reach.
	const floored = producer.subscribe({
		ranges: [{ start: { time: 3000n } }],
		delay: { max: Duration.millis(60_000) },
	});
	expect((await floored.recvGroup())?.start).toBe(3000n);
	expect((await floored.recvGroup())?.start).toBe(4000n);

	// Naming group 1000 at real time still delivers the live edge alone: the zero budget
	// calls everything older stale.
	const named = producer.subscribe({ delay: { max: Duration.ZERO }, ranges: [{ start: { time: 1000n } }] });
	expect((await named.recvGroup())?.start).toBe(4000n);
});

test("the ordered cursor sheds a stale backlog", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(30_000) });
	for (const timestamp of [0, 1000, 2000, 3000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// The whole backlog is already buffered, so no read ever blocks on it: without the
	// budget here the cursor would replay four seconds of history at 1x and stay behind.
	const live = producer.subscribe().ordered();
	expect((await live.nextGroup())?.start).toBe(3000n);

	// Only what is provably too old goes. Group 0 reaches 1s, a full 2s behind the 3s
	// edge; group 1000 reaches 2s and could still present inside a 1.5s budget.
	const bounded = producer.subscribe({ delay: { max: Duration.millis(1500) } }).ordered();
	expect((await bounded.nextGroup())?.start).toBe(1000n);
	expect((await bounded.nextGroup())?.start).toBe(2000n);
	expect((await bounded.nextGroup())?.start).toBe(3000n);

	// A budget spanning the history still bursts it in full, gap-free.
	const replay = producer.subscribe({ delay: { max: Duration.millis(30_000) } }).ordered();
	for (const key of [0n, 1000n, 2000n, 3000n]) {
		expect((await replay.nextGroup())?.start).toBe(key);
	}
});

test("the budget is clamped to the publisher's maxAge", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(1500) });
	const track = producer.subscribe({ delay: { max: Duration.millis(60_000) } });

	// Spaced so group 0's reach (2s) sits 2s behind the edge (4s): outside the
	// publisher's 1.5s window, inside the subscriber's requested minute.
	for (const timestamp of [0, 2000, 4000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Asking to tolerate a minute cannot reach back past the publisher's maxAge, the same
	// clamp delivery applies.
	expect((await track.recvGroup())?.start).toBe(2000n);
});

// The stalled group needs no timestamp of its own: its reach is where its stamped
// successor begins, which the zero budget already puts out of range.
test("a stamped successor expires a group stalled before its first frame", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(5000),
		});
		const track = producer.subscribe();
		hooks.appendNext(producer); // group 0 has no timestamp

		clock.set(11_000);
		producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1000) });
		producer.finish();
		producer.close();

		expect((await track.recvGroup())?.start).toBe(1000n);
		expect(track.recvGroup()).resolves.toBeUndefined();
	} finally {
		clock.installed.close();
	}
});

test("real time reads a live stream without truncating it", async () => {
	// 2s GOPs produced one at a time and read as they arrive, at the default (zero)
	// budget. Taking the live edge must not shorten the group the reader is already on,
	// so every frame of every group arrives.
	//
	// The reader is parked at its group's end when the next one opens, because a group's
	// close and the next group's first frame are separate events. A zero budget gives up
	// there, and the group fails with Old since its producer never closed it, but only
	// after the reader took every frame.
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(60_000) });
	const track = producer.subscribe();

	let open = hooks.appendNext(producer);
	open.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(0) });
	open.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(1900) });

	let reading = await track.recvGroup();
	const read: Array<[bigint, number]> = [];

	for (let n = 1; n < 5; n++) {
		if (!reading) throw new Error("missing live group");
		const key = reading.start;

		let frames = 0;
		while (reading.tryReadFrame()) frames++;
		read.push([key, frames]);

		// Parked at the end of the current group, with no close yet. Settled now, since it
		// fails before anything awaits it.
		const end = reading.readFrame().then(
			() => undefined,
			(err: unknown) => err,
		);

		// The next keyframe opens its group. The verdict is taken in this window, before
		// the previous group's close lands: give the parked read a real turn to run.
		const next = hooks.appendNext(producer);
		next.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(n * 2000) });
		await settle();
		open.finish();
		next.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(n * 2000 + 1900) });

		expect(await end).toMatchObject({ code: StreamCode.Old });

		reading = await track.recvGroup();
		open = next;
	}

	expect(read).toEqual([
		[0n, 2],
		[1n, 2],
		[2n, 2],
		[3n, 2],
	]);
});

test("a budget is measured from the reader's position", async () => {
	// A 2s GOP with a 1s budget: the reader has drained to 1900ms when the next group
	// opens at 2000ms, so it is 100ms behind the live edge and well inside what it asked
	// for. A straggling frame of the old group must still reach it. Measuring from the
	// group's first frame instead makes the drift 2000ms, so a budget shorter than one
	// GOP would drop the tail of every GOP.
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(60_000) });
	const track = producer.subscribe({ delay: { max: Duration.millis(1000) } });

	const open = hooks.appendNext(producer);
	open.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(0) });
	open.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(1900) });

	const reading = await track.recvGroup();
	if (!reading) throw new Error("missing group");
	expect(reading.tryReadFrame()).toBeDefined();
	expect(reading.tryReadFrame()).toBeDefined();

	// Parked at 1900ms, then the next GOP opens 100ms ahead of it.
	const late = reading.readFrame();
	const next = hooks.appendNext(producer);
	next.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(2000) });
	await settle();

	// A straggler from the old group, still inside the budget.
	open.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(1950) });
	const frame = await late;
	expect(frame?.timestamp?.asMillis()).toBe(1950);
});

test("a handed-out group still expires while its first frame is stalled", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(5000),
		});
		const track = producer.subscribe();
		hooks.appendNext(producer);

		const stalled = await track.recvGroup();
		expect(stalled?.start).toBe(0n);
		if (!stalled) throw new Error("missing stalled group");
		const pending = stalled.readFrame();
		const closed = stalled.closed;

		clock.set(11_000);
		producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1000) });

		// The producer never closed it, so even an empty prefix is not the group.
		await expect(pending).rejects.toMatchObject({ code: StreamCode.Old });
		expect(await closed).toMatchObject({ code: StreamCode.Old });
	} finally {
		clock.installed.close();
	}
});

test("committing track info wakes a group newly past the publisher's maxAge", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(10_000) } });
	const source = hooks.appendNext(producer);
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect((await group.readFrame())?.timestamp?.asMillis()).toBe(0);
	const waiting = group.readFrame();

	// Appended at the next keys: a timed start has no key before the track's timescale is known.
	appendAt(producer, 1_000, "edge");
	// A group beyond the edge, so group 0's reach (1s) is provably behind it: a group is
	// bounded by where its successor begins, so the successor alone never convicts it.
	appendAt(producer, 2_000, "later");
	await settle();
	producer.accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });

	await expect(waiting).rejects.toMatchObject({ code: StreamCode.Old });
});

test("a handed-out group reports its expiry to a writer racing it", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe();
	appendFrozen(producer, "old");

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	const write = new Promise<void>(() => {});
	const raced = race([write, group.expired]);
	appendFrozen(producer, "new");

	expect(await raced).toMatchObject({ code: StreamCode.Old, message: expect.stringContaining("max delay budget") });
	expect(group.expired.peek()).toMatchObject({ code: StreamCode.Old });
});

test("a frame still being written is judged against the budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
	const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
	const source = hooks.appendNext(producer);
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });
	source.writeFrame({ payload: enc.encode("future"), timestamp: Timestamp.fromMillis(10_000) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readFrame()).toBeDefined();
	const raced = race([new Promise<void>(() => {}), group.expired]);

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	// A group beyond the edge, so group 0's reach (1s) is provably behind it: a group is
	// bounded by where its successor begins, so the successor alone never convicts it.
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(2_000) });
	expect(await raced).toMatchObject({ code: StreamCode.Old });
});

// Mirrors the Rust `a_fully_read_group_still_reports_its_expiry`: a publisher still draining a
// group it read whole abandons the stream once the budget gives up on the group.
test("a fully read group still reports its expiry", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		const track = producer.subscribe({ delay: { max: Duration.millis(1000) } });
		const source = hooks.appendNext(producer);
		source.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readFrame()).toBeDefined();
		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
		expect(group.expired.peek()).toBeUndefined();

		// Frozen media time, so only the wall clock since the successor arrived convicts it.
		appendFrozen(producer, "b");
		const raced = race([new Promise<void>(() => {}), group.expired]);
		await settle();
		clock.set(10_999);
		await settle();
		expect(group.expired.peek()).toBeUndefined();

		clock.set(11_000);
		expect(await raced).toMatchObject({ code: StreamCode.Old });
		expect(group.expired.peek()).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a drained group finishes cleanly after the live edge advances", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe();
	appendFrozen(producer, "old");

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	appendFrozen(producer, "new");

	expect(await group.readFrame()).toBeUndefined();
	expect(group.done).toBe(true);
});

// A stalled track serves its newest group and skips the ones before it by the wall clock:
// maxAge measures how long ago each group's successor arrived as well as media time, so a
// publisher that stops writing cannot keep its superseded groups deliverable. The cache still
// holds them, since two seconds is well inside its idle window: this is staleness, not eviction.
test("a stalled track serves only its newest group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(1000) });
	try {
		for (const millis of [0, 10, 20]) appendAt(producer, millis);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([0n, 1n, 2n]);

		// Nothing is written again: media time stands still.
		clock.set(12_000);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([2n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// The last group, finished whole, ages from one mean frame interval past its highest frame
// timestamp. One only closed may lack later frames, so it never ages without a successor. Mirrors
// the Rust `a_whole_group_ages_from_its_highest_frame`.
test.each([true, false])("a last group finished whole=%s ages from its highest frame", (whole) => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.ZERO });
	try {
		const group = producer.appendGroup(Timestamp.fromMillis(0));
		// Out of order, as B-frames are: the highest is not the last written.
		for (const millis of [0, 40, 20]) group.writeFrame(textFrame("x", Timestamp.fromMillis(millis)));
		if (whole) group.finishWhole();
		else group.finish();

		// Three frames over 40ms: a 20ms mean interval, so it presents until 60ms.
		clock.set(10_059);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([0n]);
		clock.set(10_060);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual(whole ? [] : [0n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// A whole group whose frames measure no interval, one frame here, says nothing about how long it
// presents, so like a closed one it never ages without a successor. Mirrors the Rust
// `a_whole_group_of_one_frame_never_ages_alone`.
test("a whole group of one frame never ages alone", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.ZERO });
	try {
		const group = producer.appendGroup(Timestamp.fromMillis(0));
		group.writeFrame(textFrame("x", Timestamp.fromMillis(0)));
		group.finishWhole();
		clock.set(11_000);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([0n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// A peer whose timestamps barely advance never ages a group out on media time. The wall
// clock still does.
test("frozen timestamps still age out by wall clock", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const frame = () => appendFrozen(producer);
	try {
		frame();
		clock.set(10_500);
		frame();
		clock.set(11_300);
		frame();

		// Group 0's successor arrived 800ms ago, group 1's just now.
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([0n, 1n, 2n]);
		clock.set(11_500);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([1n, 2n]);
		clock.set(12_500);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([2n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// Groups that arrive at once are judged by media time, which convicts them before the wall
// clock can.
test("a startup burst ages out by media time", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		for (let second = 0; second < 5; second++) appendAt(producer, second * 1000);
		// Group N reaches N + 1 seconds against the 4s edge: groups 0 and 1 are 3s and 2s
		// behind, past 1.5s. Their successors arrived just now.
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1500) } }))).toEqual([2n, 3n, 4n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// A read parked on a superseded group is released by the wall clock alone: nothing is
// written after its successor arrives, so no write could wake it.
test("a parked read wakes at its wall deadline", async () => {
	for (const timescale of [undefined, Timescale.MILLI]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale, maxAge: null });
		try {
			const track = producer.subscribe({ delay: { max: Duration.millis(1000) } });
			const held = hooks.appendNext(producer);
			held.writeFrame({
				payload: enc.encode("x"),
				timestamp: timescale ? Timestamp.fromMillis(0) : Timestamp.unscaled(held.start),
			});
			const group = track.tryRecvGroup();
			if (!group) throw new Error("missing group");
			expect(await group.readString()).toBe("x");

			// The successor never presents a frame, so media time never bounds the held group.
			hooks.appendNext(producer);
			let woken = false;
			const read = group.readFrame().then(
				() => undefined,
				(err: unknown) => {
					woken = true;
					return err;
				},
			);
			await Promise.resolve();

			await clock.advance(10_900);
			expect(woken).toBe(false);
			await clock.advance(11_000);
			expect(await read).toMatchObject({ code: StreamCode.Old });
		} finally {
			producer.finish();
			producer.close();
			clock.installed.close();
		}
	}
});

// Evicting a recorded successor nobody read leaves its mirror a reader-side gap, which is not
// a failure: the held group is still judged by that successor's arrival, not the next one's.
test("an evicted successor with unread frames still starts the wall clock", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		const budget = 2 * CACHE_WINDOW_MS;
		const track = producer.subscribe({ delay: { max: Duration.millis(budget) } });
		// Frozen media time, so only the wall clock convicts.
		const write = () => appendFrozen(producer);
		write();
		const group = track.tryRecvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("x");

		let stale = false;
		const guarded = race([new Promise<never>(() => {}), group.expired]).then((err) => {
			stale = true;
			return err;
		});
		write(); // the successor, left unread
		await settle();
		// A later live edge, so the successor ages out of the cache.
		await clock.advance(20_000);
		write();

		await clock.advance(10_000 + budget - 1);
		expect(stale).toBe(false);
		await clock.advance(10_000 + budget);
		expect(stale).toBe(true);
		expect(await guarded).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// A held group's recorded successor that failed hands the wall clock on to the next live
// group, even when the write that brings that group in also evicts the failed one before the
// guard looks again, rather than convicting by the failed one's arrival.
test("a failed successor hands the wall clock on after the cache evicts it", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		const budget = 2 * CACHE_WINDOW_MS;
		const track = producer.subscribe({ delay: { max: Duration.millis(budget) } });
		const held = hooks.appendNext(producer);
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
		held.finish();
		const group = track.tryRecvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("x");

		let stale = false;
		const guarded = race([new Promise<never>(() => {}), group.expired]).then((err) => {
			stale = true;
			return err;
		});
		const failed = hooks.appendNext(producer);
		await settle();

		// Past the cache window, so the next write evicts the failed successor at once.
		const replacement = 10_000 + CACHE_WINDOW_MS + 10_000;
		await clock.advance(replacement);
		failed.abort(new Error("upstream failed"));
		const live = hooks.appendNext(producer);
		// Frozen media time, presenting at its key, so only the wall clock convicts.
		live.writeFrame({ payload: enc.encode("y"), timestamp: Timestamp.fromMillis(Number(live.start)) });
		live.finish();

		await clock.advance(10_000 + budget + 1);
		expect(stale).toBe(false);
		await clock.advance(replacement + budget - 1);
		expect(stale).toBe(false);
		await clock.advance(replacement + budget);
		expect(stale).toBe(true);
		expect(await guarded).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// Empty groups are handed out like any other and retired by the wall clock like an untimed
// one, whether they finished without a frame or are still open with none. A publisher
// flooding a run of them cannot pin the group before the run either.
test("a run of empty groups ages out by wall clock", () => {
	const RUN = 1000;
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		const track = producer.subscribe({ delay: { max: Duration.millis(1000) } });
		producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
		for (let key = 1; key <= RUN; key++) {
			const group = hooks.appendNext(producer);
			if (key % 2 === 0) group.finish();
		}
		appendAt(producer, 10);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toHaveLength(RUN + 2);

		clock.set(11_000);
		expect(drain(track)).toEqual([BigInt(RUN + 1)]);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1000) } }))).toEqual([BigInt(RUN + 1)]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a live track retains its newest closed group through idle expiry", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
	try {
		// A late older group must not replace the protected newest snapshot.
		for (const key of [2n, 1n]) {
			const group = producer.createGroup(key);
			group.writeFrame({ payload: enc.encode("snapshot"), timestamp: Timestamp.fromMillis(Number(key)) });
			group.finish();
		}
		await clock.advance(10_000 + CACHE_WINDOW_MS + 200);
		const replay = producer.subscribe({ delay: { max: Duration.millis(100) } });
		expect(replay.tryRecvGroup()?.start).toBe(2n);
		expect(replay.tryRecvGroup()).toBeUndefined();
		replay.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// Retention is the wall-clock half of the split: it reclaims idle content on its own
// schedule, and a reader that loses unread frames to it sees a gap. The subscription
// budget is timestamp-only and says nothing here (the empty successor has no timestamp
// to bound the held group with).
test("retention eviction surfaces as a gap for a handed-out group", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		const source = hooks.appendNext(producer);
		source.writeFrame(textFrame("first", Timestamp.fromMillis(Number(source.start))));
		source.writeFrame(textFrame("tail", Timestamp.fromMillis(Number(source.start))));
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		hooks.appendNext(producer);

		await expect(group.readFrame()).rejects.toThrow(TooFarBehind);
	} finally {
		clock.installed.close();
	}
});

test("idle expiry reclaims an ended track's held mirror", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		const source = hooks.appendNext(producer);
		source.writeFrame(textFrame("first", Timestamp.fromMillis(Number(source.start))));
		source.writeFrame(textFrame("tail", Timestamp.fromMillis(Number(source.start))));
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		producer.finish();
		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ delay: { max: Duration.millis(100) } });

		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
	} finally {
		clock.installed.close();
	}
});

test("retention reclaims a group the publisher abandoned open", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		const stalled = hooks.appendNext(producer);
		stalled.writeFrame(textFrame("first", Timestamp.fromMillis(Number(stalled.start))));
		// A successor, so the stalled group is not the live edge the publisher is
		// still filling. It is never closed: the publisher simply walked away.
		hooks.appendNext(producer);

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");

		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ delay: { max: Duration.millis(100) } });

		// A gap, not a clean finish: the publisher never ended this group.
		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
	} finally {
		clock.installed.close();
	}
});

test("an idle live edge ages out once a newer group arrives", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		const edge = hooks.appendNext(producer);
		edge.writeFrame(textFrame("first", Timestamp.fromMillis(Number(edge.start))));

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");

		// A prune while it is still the live edge keeps it, and finds nothing else to age out.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ delay: { max: Duration.millis(100) } });

		// A successor ends the exemption, and the group is long past the window, so the
		// write evicts it rather than a later wakeup.
		clock.set(10_000 + CACHE_WINDOW_MS + 4_000);
		hooks.appendNext(producer);
		const read = group.readFrame().then(
			() => "clean end",
			(err: unknown) => err,
		);
		expect(await Promise.race([read, settle().then(() => "still parked")])).toBeInstanceOf(TooFarBehind);
	} finally {
		clock.installed.close();
	}
});

test("an abandoned open group ages out with no further write", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		// A budget past the cache window, so the eviction rather than the budget ends it.
		const track = producer.subscribe({ delay: { max: Duration.millis(2 * CACHE_WINDOW_MS) } });
		const stalled = hooks.appendNext(producer);
		stalled.writeFrame(textFrame("first", Timestamp.fromMillis(Number(stalled.start))));
		hooks.appendNext(producer);

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");
		const read = group.readFrame().catch((err: unknown) => err);
		await clock.advance(10_000 + CACHE_WINDOW_MS + SWEEP_LAG_MS);
		expect(await read).toBeInstanceOf(TooFarBehind);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("publisher maxAge does not change the idle cache deadline", async () => {
	for (const maxAge of [Duration.millis(30), Duration.millis(2 ** 31 + 10_000)]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge });
		try {
			const idle = hooks.appendNext(producer);
			idle.writeFrame(textFrame("cached", Timestamp.fromMillis(Number(idle.start))));
			hooks.appendNext(producer);
			// Eviction ends the idle open group a cache window after its last write, give or take
			// the lag of the pool's sweep.
			await clock.advance(10_000 + CACHE_WINDOW_MS - 1);
			expect(idle.closed.peek()).toBeUndefined();
			await clock.advance(10_000 + CACHE_WINDOW_MS + SWEEP_LAG_MS);
			expect(idle.closed.peek()).toBeInstanceOf(TooFarBehind);
		} finally {
			producer.finish();
			producer.close();
			clock.installed.close();
		}
	}
});

test("retention pruning preserves clean EOF for a drained mirror", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		const source = hooks.appendNext(producer);
		source.writeFrame(textFrame("only", Timestamp.fromMillis(Number(source.start))));
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("only");
		expect(await group.readFrame()).toBeUndefined();

		producer.finish();
		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ delay: { max: Duration.millis(100) } });

		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
	} finally {
		clock.installed.close();
	}
});

test("system clock changes do not affect the latency budget", async () => {
	const clock = manualTime(10_000);
	clock.stepWall(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(100) });
		const track = producer.subscribe({ delay: { max: Duration.millis(100) } });
		appendFrozen(producer, "old");

		clock.stepWall(20_000);
		appendFrozen(producer, "new");

		expect((await track.recvGroup())?.start).toBe(0n);
		expect((await track.recvGroup())?.start).toBe(1n);
	} finally {
		clock.installed.close();
	}
});

test("frame readiness cancels the losing latency waiter", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const source = hooks.appendNext(producer);
	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");

	for (let i = 0; i < 110; i++) {
		const pending = group.readFrame();
		source.writeFrame(textFrame(`${i}`, Timestamp.fromMillis(Number(source.start))));
		expect(new TextDecoder().decode((await pending)?.payload)).toBe(`${i}`);
	}
});

test("latency budget retains a consumed live-edge anchor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe();

	const edge = producer.createGroup(2n);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.finish();
	expect((await track.recvGroup())?.start).toBe(2n);

	const late = producer.createGroup(1n);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(0) });
	late.finish();
	producer.finish();
	producer.close();

	expect(track.recvGroup()).resolves.toBeUndefined();
});

test("an aborted live-edge anchor does not make older content stale", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe();

	const edge = producer.createGroup(2n);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.abort(new Error("aborted"));

	const older = producer.createGroup(1n);
	older.writeFrame({ payload: enc.encode("older"), timestamp: Timestamp.fromMillis(0) });
	older.finish();

	expect((await track.recvGroup())?.start).toBe(1n);
	track.close();
});

test.each(["arrival", "ordered"] as const)("subscription bounds stay reader-local for %s reads", async (order) => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const subscriber = producer.subscribe({
		delay: { max: Duration.millis(5000) },
		ranges: [{ start: { time: 1n }, end: { time: 2n } }],
	});
	const ordered = order === "ordered" ? subscriber.ordered() : undefined;
	const read = () => (ordered ? ordered.nextGroup() : subscriber.recvGroup());
	try {
		for (let key = 0n; key < 5n; key++) producer.createGroup(key);
		expect((await everything.recvGroup())?.start).toBe(0n);
		expect((await read())?.start).toBe(1n);
		expect((await read())?.start).toBe(2n);
		// A bounded read ends once its window is read, as in Rust, though the track goes on.
		expect(await read()).toBeUndefined();
		// Replacing the ranges reads on from the same reader.
		subscriber.update({
			delay: { max: Duration.millis(5000) },
			ranges: [{ start: { time: 3n }, end: { time: 3n } }],
		});
		expect((await read())?.start).toBe(3n);
		expect(await read()).toBeUndefined();
		subscriber.update({ delay: { max: Duration.millis(5000) }, ranges: [{ end: { time: 4n } }] });
		// Arrival readers can now offer the unread group below the old floor; ordered ones cannot rewind.
		if (!ordered) expect((await read())?.start).toBe(0n);
		expect((await read())?.start).toBe(4n);
		expect(await read()).toBeUndefined();
		// An open range parks at the live edge instead.
		subscriber.update({ delay: { max: Duration.millis(5000) }, ranges: [{ start: { time: 5n } }] });
		const pending = read();
		expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");
		producer.finish();
		producer.close();
		expect(await pending).toBeUndefined();
	} finally {
		subscriber.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("subscription updates narrow the cursor before its first read", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	try {
		for (let key = 0n; key < 5n; key++) producer.createGroup(key);
		track.update({
			delay: { max: Duration.millis(5000) },
			ranges: [{ start: { time: 3n }, end: { time: 3n } }],
		});
		expect((await track.recvGroup())?.start).toBe(3n);
		// Its window read, the bounded read ends.
		expect(await track.recvGroup()).toBeUndefined();
	} finally {
		track.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("a subscription cap chooses its own live edge", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const everything = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const track = producer.subscribe({ delay: { max: Duration.ZERO }, ranges: [{ end: { time: 1n } }] });
	try {
		for (let key = 0; key < 5; key++) appendAt(producer, key * 1000, `${key}`);
		expect((await track.recvGroup())?.start).toBe(1n);
		track.update({ delay: { max: Duration.ZERO }, ranges: [{ start: { time: 3n }, end: { time: 3n } }] });
		expect((await track.recvGroup())?.start).toBe(3n);
	} finally {
		track.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("withRange with an excluded end of 0 is the empty range and does not ride another subscriber's demand", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const empty = producer.subscribe({ delay: { max: Duration.millis(5000) } }).withRange({ end: { excluded: 0n } });

	for (let key = 0n; key < 3n; key++) producer.createGroup(key);

	expect((await everything.recvGroup())?.start).toBe(0n);
	const pending = empty.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	empty.withRange({ end: { excluded: 2n } });
	expect((await pending)?.start).toBe(0n);
	expect((await empty.recvGroup())?.start).toBe(1n);

	empty.withRange({});
	expect((await empty.recvGroup())?.start).toBe(2n);
});

// Completion depends on unread groups inside the range, even when the cache holds more.
for (const ordered of [false, true]) {
	for (const end of [0n, 3n, 4n]) {
		test(`a clean close finishes a capped ${ordered ? "ordered" : "arrival"} reader at ${end}`, () => {
			const producer = new TrackProducer("test");
			const subscriber = producer.subscribe({ delay: { max: null } }).withRange({ end: { excluded: end } });
			if (ordered) subscriber.ordered();
			const read = () => (ordered ? hooks.tryNextGroup(subscriber) : hooks.tryRecvGroup(subscriber, false));
			for (const key of [0n, 2n, 4n]) producer.createGroup(key).finish();
			producer.finish();
			producer.close();

			for (const key of [0n, 2n].filter((key) => key < end)) {
				expect(read()).toMatchObject({ kind: "group", group: { start: key } });
			}
			expect(read()).toEqual({ kind: "done" });

			// Ending a read preserves the cache for a later widened range.
			subscriber.withRange({});
			for (const key of [0n, 2n, 4n].filter((key) => key >= end)) {
				expect(read()).toMatchObject({ kind: "group", group: { start: key } });
			}
			expect(read()).toEqual({ kind: "done" });
			subscriber.close();
		});
	}
}

test("withRange caps the live edge used by the latency budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe();
	track.withRange({ end: { excluded: 2n } });

	for (const timestamp of [0, 1000, 2000]) appendAt(producer, timestamp, `${timestamp}`);

	expect((await track.recvGroup())?.start).toBe(1n);
	track.withRange({});
	expect((await track.recvGroup())?.start).toBe(2n);
});

// The frame helpers ride the group cursor, so its bounds apply to them too: a group
// above the cap stays cached across a clean close until the cap admits it.
test("withRange caps frame-level reads like the group cursor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(5000) });
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();
	track.withRange({ end: { excluded: 1n } });

	producer.writeFrame({ payload: enc.encode("zero"), timestamp: Timestamp.fromMillis(0) });
	producer.writeFrame({ payload: enc.encode("one"), timestamp: Timestamp.fromMillis(1) });
	producer.finish();
	producer.close();

	expect((await track.readFrame())?.group).toBe(0n);

	// The current range is finished; widening it still offers the cached frames.
	expect(await track.readFrame()).toBeUndefined();
	track.withRange({ end: { excluded: 2n } });
	expect((await track.readFrame())?.group).toBe(1n);
	expect(await track.readFrame()).toBeUndefined();
});

test("local cursor bounds can skip, pause, and release buffered groups", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();

	for (let key = 0n; key < 5n; key++) producer.createGroup(key);

	track.withRange({ start: { included: 1n }, end: { excluded: 3n } });
	expect((await track.nextGroup())?.start).toBe(1n);
	expect((await track.nextGroup())?.start).toBe(2n);

	const pending = track.nextGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ start: { included: 4n }, end: { excluded: 5n } });
	expect((await pending)?.start).toBe(4n);
});

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("recvGroup serves a late arrival after a newer group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });

	producer.createGroup(2n);
	expect((await track.recvGroup())?.start).toBe(2n);

	// Group 1 lands after group 2 was already served.
	producer.createGroup(1n);
	expect((await track.recvGroup())?.start).toBe(1n);

	// Staleness is the latency window's job, not arrival order's: the track
	// still finishes normally afterward.
	producer.finish();
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
});

// Both cursors retain beyond-cap groups for widening, even after a clean close.
test("withRange parks recvGroup beyond the cap and a raised cap re-offers", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });

	for (let key = 0n; key < 3n; key++) producer.createGroup(key).finish();

	track.withRange({ end: { excluded: 2n } });
	expect((await track.recvGroup())?.start).toBe(0n);
	expect((await track.recvGroup())?.start).toBe(1n);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	// A clean close ends the pending read without discarding the cached group.
	producer.finish();
	producer.close();
	expect(await pending).toBeUndefined();

	track.withRange({});
	expect((await track.recvGroup())?.start).toBe(2n);
	expect(await track.recvGroup()).toBeUndefined();
});

// A group beyond the cap must not block in-range groups that arrive behind it:
// a relay can ingest a burst micro-reordered (newest first).
test("recvGroup serves in-range groups that arrive behind a capped one", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });

	track.withRange({ end: { excluded: 2n } });

	// Reordered burst: the beyond-cap group arrives first.
	producer.createGroup(2n);
	producer.createGroup(0n);
	producer.createGroup(1n);

	expect((await track.recvGroup())?.start).toBe(0n);
	expect((await track.recvGroup())?.start).toBe(1n);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ end: { excluded: 3n } });
	expect((await pending)?.start).toBe(2n);
});

// A raised floor drops parked groups it overtook instead of re-offering them once the cap
// rises.
test("withRange drops groups recvGroup parked at the cap", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.withRange({ end: { excluded: 1n } });
	producer.createGroup(1n);
	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ start: { included: 2n } });
	producer.createGroup(2n);

	// The overtaken parked group is dropped, not re-offered.
	expect((await pending)?.start).toBe(2n);
});

// A live duplicate would fan out to every subscriber twice (recvGroup has no sequence
// cursor to hide it), so createGroup rejects it like Rust's claim_sequence. An aborted
// incarnation is evicted so a fresh group can serve the sequence again.
test("createGroup rejects a duplicate live sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	const first = producer.createGroup(7n);
	first.writeFrame(textFrame("first", Timestamp.fromMillis(Number(first.start))));
	first.finish();

	// A peer re-sending a live sequence is rejected, not fanned out again.
	expect(() => producer.createGroup(7n)).toThrow("duplicate group");
	expect((await track.recvGroup())?.start).toBe(7n);

	// An aborted incarnation is replaceable: the retry serves the sequence fresh.
	const aborted = producer.createGroup(8n);
	aborted.abort(new Error("upstream reset"));

	const retry = producer.createGroup(8n);
	retry.writeFrame(textFrame("retry", Timestamp.fromMillis(Number(retry.start))));
	retry.finish();

	const got = await track.recvGroup();
	expect(got?.start).toBe(8n);
	expect(await got?.readString()).toBe("retry");
});

// A beyond-cap group outlives a clean close for a later widening, so the subscriber
// leaving is what releases it: widening after close() finds nothing.
test("closing the subscriber releases a group retained across a clean close", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });

	track.withRange({ end: { excluded: 1n } });
	producer.createGroup(1n).finish();

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	producer.finish();
	producer.close();
	expect(await pending).toBeUndefined();

	track.close();
	track.withRange({});
	expect(await track.recvGroup()).toBeUndefined();
});

// Same, but with the producer still live: the first close() must also clear the
// buffer, or the parked group re-parks on wake and the read hangs forever.
test("closing the subscriber releases a recvGroup parked while the producer is live", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.withRange({ end: { excluded: 1n } });
	producer.createGroup(1n);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.close();
	expect(await pending).toBeUndefined();
});

test("the ordered and arrival cursors are independent", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();
	const arrival = producer.subscribe({ delay: { max: Duration.millis(5000) } });

	producer.createGroup(5n);
	expect((await ordered.nextGroup())?.start).toBe(5n);

	// A late seq 3 is skipped by the ordered cursor, which has already passed it, but the
	// arrival cursor has both buffered and still delivers each exactly once.
	producer.createGroup(3n);
	expect((await arrival.recvGroup())?.start).toBe(3n);
	expect((await arrival.recvGroup())?.start).toBe(5n);
});

// Both cursors draw from one buffer, so interleaving them produces a stream in neither
// order. The first group read commits the subscription; the other order is refused.
test("the first group read commits the cursor and refuses the other order", async () => {
	const producer = new TrackProducer("test");

	// An arrival read commits to arrival order; ordered() is refused afterwards.
	const arrival = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	producer.createGroup(5n);
	expect((await arrival.recvGroup())?.start).toBe(5n);
	expect(() => arrival.ordered()).toThrow("arrival order");

	// The commitment refuses the other cursor without poisoning this one: a late
	// lower sequence still flows in arrival order.
	producer.createGroup(3n);
	expect((await arrival.recvGroup())?.start).toBe(3n);

	// The other direction: ordered() takes the subscription and arrival reads throw.
	const inert = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	const ordered = inert.ordered();
	expect(inert.recvGroup()).rejects.toThrow("sequence order");
	expect(() => inert.ordered()).toThrow("sequence order");
	expect((await ordered.nextGroup())?.start).toBe(3n);
	expect((await ordered.nextGroup())?.start).toBe(5n);
});

// Datagrams are a separate cursor on either handle, so reading them must not commit

test("nextGroup returns undefined when track closes", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();
	producer.finish();
	producer.close();
	expect(await track.nextGroup()).toBeUndefined();
});

// A clean close ends a pending ordered read without erasing beyond-cap frames.
test("a closed track still delivers a group parked above the cap once the cap is raised", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).ordered();

	for (let key = 0n; key < 3n; key++) {
		const group = producer.createGroup(key);
		group.writeFrame(textFrame(`frame-${key}`, Timestamp.fromMillis(Number(group.start))));
		group.finish();
	}

	track.withRange({ end: { excluded: 2n } });
	expect((await track.nextGroup())?.start).toBe(0n);
	expect((await track.nextGroup())?.start).toBe(1n);

	// Group 2 parks while open; a clean close completes the current range.
	const parked = track.nextGroup();
	producer.finish();
	producer.close();
	expect(await parked).toBeUndefined();

	// Raising the cap after close releases the buffered group, frames intact.
	track.withRange({ end: { excluded: 3n } });
	const released = await track.nextGroup();
	expect(released?.start).toBe(2n);
	expect(await released?.readString()).toBe("frame-2");

	// Drained and closed: now it's finished.
	expect(await track.nextGroup()).toBeUndefined();
});

// 0 is the only encoding for "nothing produced"; an abort declares no boundary at all.
test("end is group 0 for an empty track and undefined after an abort", async () => {
	const empty = new TrackProducer("test");
	const emptyTrack = empty.subscribe();
	empty.finish();
	empty.close();
	expect(hooks.subscriberEnd(emptyTrack)).toEqual({ group: 0n, frame: 0 });

	const aborted = new TrackProducer("test");
	const abortedTrack = aborted.subscribe();
	aborted.abort(new Error("boom"));
	expect(hooks.subscriberEnd(abortedTrack)).toBeUndefined();
});

test("readFrame does not livelock when a sole group finishes before the next arrives", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();

	// A group is appended then finished empty while the track stays open. A finished group's
	// readable() resolves immediately, so the reader must not busy-wait on it (which would starve the
	// macrotask queue and never observe the next group).
	const g0 = hooks.appendNext(producer);
	g0.finish();

	// The next group arrives in a later host task; if the reader livelocks on microtasks it never runs.
	void yieldNow().then(() => {
		const g1 = hooks.appendNext(producer);
		g1.writeFrame(textFrame("hello", Timestamp.fromMillis(Number(g1.start))));
		g1.finish();
		producer.finish();
		producer.close();
	});

	expect(await track.readString()).toBe("hello");
}, 2000);

// The IETF publisher resolves Largest Object from this, so it has to name a frame, not just
// a group: a filter is applied down to the object.
test("largest names the newest frame written", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1_000) } });

	// Nothing published yet, so the first read waits.
	expect(producer.largest()).toBeUndefined();
	const initial = subscriber.largest();
	expect(await Promise.race([initial.then(() => "ready"), Promise.resolve("pending")])).toBe("pending");

	const first = hooks.appendNext(producer);
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
	first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(1) });
	expect(await initial).toEqual({ group: 0n, frame: 1 });
	// The producer's getter holds no cursor, so every caller reads the same edge.
	expect(producer.largest()).toEqual({ group: 0n, frame: 1 });
	expect(producer.largest()).toEqual({ group: 0n, frame: 1 });
	first.finish();

	// A new group raises the largest at its head before any frame, so its first frame names
	// nothing newer.
	const second = hooks.appendNext(producer);
	expect(await subscriber.largest()).toEqual({ group: 1n, frame: 0 });

	second.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.fromMillis(1000) });
	let advanced = false;
	const next = subscriber.largest().then((largest) => {
		advanced = true;
		return largest;
	});
	await settle();
	expect(advanced).toBe(false);
	second.writeFrame({ payload: enc.encode("d"), timestamp: Timestamp.fromMillis(1001) });
	expect(await next).toEqual({ group: 1n, frame: 1 });
	second.finish();

	// Reading a group does not move the edge: it is what the track produced, not what is left.
	await subscriber.recvGroup();
	const ended = subscriber.largest();
	producer.finish();
	producer.close();
	expect(await ended).toBeUndefined();
});

// A subscriber that arrives after the frames were written reads mirrors. largest is the
// last frame successfully written, even after an overflow aborts the group.
test("largest ends at the declared end while the producer stays open", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();

	const group = hooks.appendNext(producer);
	group.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	expect(await subscriber.largest()).toEqual({ group: 0n, frame: 0 });

	// The end is reached, but the open group can still raise the largest.
	producer.finishAt({ group: 1n, frame: 0 });
	const ended = subscriber.largest();
	expect(await Promise.race([ended.then(() => "ready"), Promise.resolve("pending")])).toBe("pending");

	group.finish();
	expect(await ended).toBeUndefined();
	producer.finish();
	producer.close();
});

// Every sequence below a reached end already raised the largest, so a late group below it
// changes nothing, and the ended stream stays ended.
test("a late group below the largest changes nothing", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();

	const framed = producer.createGroup(2n);
	framed.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(2) });
	framed.finish();
	const empty = producer.createGroup(5n);
	empty.finish();
	producer.finishAt({ group: 6n, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 5n, frame: 0 });
	expect(await subscriber.largest()).toBeUndefined();

	// Late content below the end is still admitted; it just isn't the largest.
	const late = producer.createGroup(4n);
	late.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(4) });
	expect(await subscriber.largest()).toBeUndefined();
	expect(await producer.subscribe().largest()).toEqual({ group: 5n, frame: 0 });
	producer.finish();
	producer.close();
});

// An aborted largest group can still be recreated, so the stream stays open past the end
// until a copy of it closes cleanly, and the recreated frames are reported before it ends.
test("a recreated largest group is reported before the end", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	// The recreated group carries the same content, timestamps included.
	const timestamp = Timestamp.now();
	const first = producer.createGroup(0n);
	first.writeFrame({ payload: enc.encode("a"), timestamp });
	first.abort(new Error("cut"));
	producer.finishAt({ group: 1n, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 0n, frame: 0 });
	let advanced = false;
	const next = subscriber.largest().then((largest) => {
		advanced = true;
		return largest;
	});
	await settle();
	expect(advanced).toBe(false);

	const again = producer.createGroup(0n);
	for (const payload of ["a", "b"]) again.writeFrame({ payload: enc.encode(payload), timestamp });
	expect(await next).toEqual({ group: 0n, frame: 1 });
	again.finish();
	expect(await subscriber.largest()).toBeUndefined();
	producer.finish();
	producer.close();
});

/** Write untimed group `sequence` with `frames` frames, left open. */
function framedGroup(producer: TrackProducer, key: bigint, frames: number): GroupProducer {
	const group = producer.createGroup(key);
	for (let i = 0; i < frames; i++)
		group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.unscaled(group.start) });
	return group;
}

/** Whether `promise` is still pending once queued work settles. */
async function isPending(promise: Promise<unknown>): Promise<boolean> {
	let settled = false;
	void promise.then(
		() => (settled = true),
		() => (settled = true),
	);
	await settle();
	return !settled;
}

// The largest reports only while it is within the bounds: a cap below it or a floor above it
// reports nothing, rather than a cached group standing in for it.
test("largest reports only within the bounds", async () => {
	const producer = new TrackProducer("test").accept({});
	for (let key = 0n; key < 6n; key++) framedGroup(producer, key, 2);

	const capped = producer.subscribe().withRange({ end: { excluded: 3n } });
	expect(await isPending(capped.largest())).toBe(true);

	// An inclusive cap reports the group it names.
	const ordered = producer
		.subscribe()
		.ordered()
		.withRange({ end: { included: 5n } });
	expect(await ordered.largest()).toEqual({ group: 5n, frame: 1 });

	const floored = producer.subscribe().withRange({ start: { included: 8n } });
	const next = floored.largest();
	expect(await isPending(next)).toBe(true);
	framedGroup(producer, 8n, 0);
	expect(await next).toEqual({ group: 8n, frame: 0 });
	producer.finish();
	producer.close();
});

// Moving the bounds wakes a pending read, which reports the largest once it is within them;
// bounds that leave it where it was report nothing new.
test("largest follows the bounds as they move", async () => {
	const producer = new TrackProducer("test").accept({});
	for (let key = 0n; key < 6n; key++) framedGroup(producer, key, 1);

	const reader = producer
		.subscribe()
		.ordered()
		.withRange({ end: { excluded: 3n } });
	let next = reader.largest();
	expect(await isPending(next)).toBe(true);
	reader.withRange({});
	expect(await next).toEqual({ group: 5n, frame: 0 });

	next = reader.largest();
	expect(await isPending(next)).toBe(true);
	reader.withRange({ end: { excluded: 2n } });
	expect(await isPending(next)).toBe(true);

	// Lifting it again leaves the last report standing.
	reader.withRange({});
	expect(await isPending(next)).toBe(true);
	framedGroup(producer, 6n, 0);
	expect(await next).toEqual({ group: 6n, frame: 0 });
	reader.withRange({ start: { included: 3n } });
	expect(await isPending(reader.largest())).toBe(true);
	producer.finish();
	producer.close();
});

// Once the stream ended, widening the bounds over a largest it never reported does not bring
// it back.
test("largest stays ended when the bounds widen", async () => {
	const producer = new TrackProducer("test").accept({});
	framedGroup(producer, 5n, 1).finish();
	producer.finish();
	producer.close();

	const reader = producer.subscribe().withRange({ end: { excluded: 3n } });
	expect(await reader.largest()).toBeUndefined();
	reader.withRange({});
	expect(await reader.largest()).toBeUndefined();
});

// A reader that starts at the largest group while it is still empty is at the live edge: it
// gets that group, which ends with nothing, then the next one.
test("a live-edge reader starts on an empty largest group", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const first = hooks.appendNext(producer);
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
	first.finish();
	const empty = hooks.appendNext(producer);

	const reader = producer.subscribe({ delay: { max: Duration.millis(30_000) } });
	const edge = await reader.largest();
	expect(edge).toEqual({ group: 1n, frame: 0 });
	reader.withRange({ start: { included: edge?.group ?? 0n } });

	empty.finish();
	const next = hooks.appendNext(producer);
	next.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(2) });

	const group = await reader.recvGroup();
	expect(group?.start).toBe(1n);
	expect(await group?.readFrame()).toBeUndefined();
	const after = await reader.recvGroup();
	expect(after?.start).toBe(2n);
	expect(await after?.readFrame()).toBeDefined();
	producer.finish();
	producer.close();
});

test("largest survives a mirror replay of a group with a shed front", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });

	const group = hooks.appendNext(producer);
	for (let i = 0; i < 40; i++) {
		group.writeFrame({ payload: new Uint8Array(1024 * 1024), timestamp: Timestamp.now() });
	}

	// Subscribing now replays into a fresh sink; shedding leaves the absolute live edge intact.
	const late = producer.subscribe();
	expect(await late.largest()).toEqual({ group: 0n, frame: 40 - 1 });

	producer.finish();
	producer.close();
});

test("group ranges preserve progress and spell out inclusion", async () => {
	const producer = new TrackProducer("test");
	const track = producer
		.subscribe({ delay: { max: Duration.millis(5000) } })
		.withRange({ start: { included: 1n }, end: { included: 1n } });
	for (let key = 0n; key < 4n; key++) producer.createGroup(key);
	expect(track.tryRecvGroup()?.start).toBe(1n);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.withRange({ start: { included: 1n }, end: { excluded: 3n } });
	expect(track.tryRecvGroup()?.start).toBe(2n);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.withRange({ start: { included: 1n } });
	expect(track.tryRecvGroup()?.start).toBe(3n);
	track.close();
});

// Mirrors the Rust `a_lowered_range_reoffers_the_unread_cached_groups`.
test("a lowered range re-offers the unread cached groups", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).withRange({ start: { included: 10n } });
	for (const key of [5n, 6n, 10n]) producer.createGroup(key);
	expect(track.tryRecvGroup()?.start).toBe(10n);
	expect(track.tryRecvGroup()).toBeUndefined();

	track.withRange({ start: { included: 6n } });
	expect(track.tryRecvGroup()?.start).toBe(6n);
	track.withRange({ end: { included: 5n } });
	expect(track.tryRecvGroup()?.start).toBe(5n);
	track.withRange({});
	expect(track.tryRecvGroup()).toBeUndefined();
	producer.createGroup(11n);
	expect(track.tryRecvGroup()?.start).toBe(11n);
	track.close();
});

test("a lowered range re-offers a cached cut with its error", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).withRange({ start: { included: 1n } });
	const cut = producer.createGroup(0n);
	const error = new StreamError(StreamCode(70), { message: "cut" });
	cut.abort(error);
	producer.createGroup(1n);
	expect(track.tryRecvGroup()?.start).toBe(1n);

	track.withRange({});
	const group = track.tryRecvGroup();
	expect(group?.start).toBe(0n);
	await expect(group?.readString()).rejects.toBe(error);
	track.close();
	producer.finish();
	producer.close();
});

test("a lowered range still re-offers after the producer closes", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } }).withRange({ start: { included: 1n } });
	for (const key of [0n, 1n]) producer.createGroup(key).finish();
	expect(track.tryRecvGroup()?.start).toBe(1n);
	producer.finish();
	producer.close();

	track.withRange({});
	expect(track.tryRecvGroup()?.start).toBe(0n);
	expect(hooks.tryRecvGroup(track, false)).toEqual({ kind: "done" });
	track.close();
});

for (const abort of [undefined, new Error("gone")]) {
	test(`a lowered range offers nothing to a reader that ${abort ? "aborted" : "closed"}`, () => {
		const producer = new TrackProducer("test");
		const track = producer
			.subscribe({ delay: { max: Duration.millis(5000) } })
			.withRange({ start: { included: 1n } });
		for (const key of [0n, 1n]) producer.createGroup(key);
		expect(track.tryRecvGroup()?.start).toBe(1n);
		track.close(abort);

		track.withRange({});
		expect(hooks.tryRecvGroup(track, false)).toEqual(abort ? { kind: "error", error: abort } : { kind: "done" });
		producer.finish();
		producer.close();
	});
}

test("malformed group bounds do not partially advance the cursor", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(5000) } });
	producer.createGroup(0n);
	expect(() => track.withRange({ start: { included: 3n }, end: { excluded: -1n } })).toThrow();
	expect(track.tryRecvGroup()?.start).toBe(0n);
	track.close();
});

test("finishAt declares an end ahead of the live edge without ending the track", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	producer.createGroup(5n);
	const first = await track.recvGroup();
	expect(first?.start).toBe(5n);

	producer.finishAt({ group: 8n, frame: 0 });
	expect(hooks.subscriberEnd(track)).toEqual({ group: 8n, frame: 0 });
	expect(await track.finished()).toEqual({ group: 8n, frame: 0 });

	// Not terminal: groups below the end still arrive, including a straggler.
	const late = producer.createGroup(7n);
	late.finish();
	const straggler = producer.createGroup(6n);
	straggler.finish();
	expect((await track.recvGroup())?.start).toBe(6n);
	expect((await track.recvGroup())?.start).toBe(7n);

	// A clean close keeps the declared end rather than the live edge.
	producer.finish();
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
	expect(hooks.subscriberEnd(track)).toEqual({ group: 8n, frame: 0 });

	// A late subscriber sees the same end.
	expect(hooks.subscriberEnd(producer.subscribe())).toEqual({ group: 8n, frame: 0 });
});

for (const ordered of [false, true]) {
	test(`a ${ordered ? "sequence" : "arrival"} reader ends at the declared boundary before closure`, async () => {
		const producer = new TrackProducer("test");
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
		const track = ordered ? subscriber.ordered() : subscriber;
		const receive = () => ("nextGroup" in track ? track.nextGroup() : track.recvGroup());
		try {
			producer.finishAt({ group: 3n, frame: 0 });
			const waiting = receive();
			const group = producer.createGroup(2n);
			group.writeFrame(textFrame("tail", Timestamp.fromMillis(Number(group.start))));
			const received = await waiting;
			expect(received?.start).toBe(2n);
			expect(await received?.readString()).toBe("tail");

			// Groups 0 and 1 are missing and the last group is still open. Neither
			// prevents a reader ending, but closure and its abort remain separate.
			let ended = false;
			const end = receive().then((value) => {
				ended = true;
				expect(value).toBeUndefined();
			});
			await Promise.resolve();
			await Promise.resolve();
			expect(ended).toBe(true);
			await end;
			expect(track.closed.peek()).toBeUndefined();

			const aborted = new Error("cut off");
			producer.abort(aborted);
			await expect(receive()).rejects.toThrow("cut off");
			await expect(received?.readString() ?? Promise.resolve()).rejects.toThrow("cut off");
		} finally {
			producer.finish();
			producer.close();
			track.close();
		}
	});
}

test("finished rejects when the track aborts or closes without an end", async () => {
	const aborted = new TrackProducer("test");
	const pending = aborted.subscribe().finished();
	aborted.abort(new Error("boom"));
	await expect(pending).rejects.toThrow("boom");

	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	track.close();
	await expect(track.finished()).rejects.toThrow();

	// A clean close is an end, so it resolves.
	const clean = new TrackProducer("test");
	const reader = clean.subscribe();
	hooks.appendNext(clean).finish();
	clean.finish();
	clean.close();
	expect(await reader.finished()).toEqual({ group: 1n, frame: 0 });
});

// Rust applies the pool's idle expiry with `max_age: None` too: no publisher limit only
// removes the media-time clamp, never the wall-clock bound on memory.
test("no publisher limit still ages idle groups out", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI, maxAge: null });
	try {
		const source = hooks.appendNext(producer);
		source.writeFrame(textFrame("old", Timestamp.fromMillis(Number(source.start))));
		source.finish();
		clock.set(10_000 + CACHE_WINDOW_MS - 1_000);
		hooks.appendNext(producer);
		const within = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		expect(await (await within.recvGroup())?.readString()).toBe("old");
		within.close();

		// The read was an access, as in Rust: past the write's window, the group stays. Taking it
		// again is another access.
		clock.set(10_000 + CACHE_WINDOW_MS + 1_000);
		const read = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		expect(read.tryRecvGroup()?.start).toBe(0n);
		read.close();

		// Past that delivery's window, a subscribe's own scan reclaims it.
		clock.set(10_000 + 2 * CACHE_WINDOW_MS + 2_000);
		const after = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		expect(after.tryRecvGroup()?.start).toBe(1n);
		expect(after.tryRecvGroup()).toBeUndefined();
		after.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a live track does not pin an aborted newest group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const good = hooks.appendNext(producer);
		good.writeFrame(textFrame("snapshot", Timestamp.fromMillis(0)));
		good.finish();
		const reset = hooks.appendNext(producer);
		reset.writeFrame(textFrame("partial", Timestamp.fromMillis(1)));
		reset.abort(new Error("reset"));

		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		const late = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("closing a track keeps an idle newest group for subscribers to drain", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("catalog").accept({ timescale: Timescale.MILLI });
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		const snapshot = hooks.appendNext(producer);
		snapshot.writeFrame(textFrame("snapshot", Timestamp.fromMillis(Number(snapshot.start))));
		snapshot.finish();

		// Idle well past the window, but protected while the track is live.
		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		producer.finish();
		producer.close();
		expect(await (await subscriber.recvGroup())?.readString()).toBe("snapshot");
		expect(await subscriber.recvGroup()).toBeUndefined();

		// The close restarted its window, which now runs out like any other.
		clock.set(10_000 + 2 * (CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8));
		const late = producer.subscribe({ delay: { max: Duration.millis(100_000) } });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		clock.installed.close();
	}
});

test("a late subscriber to a closed unlimited track is released once the cache ages out", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI });
		const source = hooks.appendNext(producer);
		source.writeFrame(textFrame("last", Timestamp.fromMillis(Number(source.start))));
		source.finish();
		producer.finish();
		producer.close();

		// Counts the map entries subscribing leaves behind, rather than the heap: any entry
		// keyed by the subscriber that outlives the cache window would pin it. Only an object key
		// can be, so the subscriber's own ranges, keyed by number, don't count.
		const added: [Map<unknown, unknown>, unknown][] = [];
		const set = Map.prototype.set;
		const spy = spyOn(Map.prototype, "set").mockImplementation(function (this: Map<unknown, unknown>, key, value) {
			added.push([this, key]);
			return set.call(this, key, value);
		});
		let subscriber: ReturnType<TrackProducer["subscribe"]>;
		try {
			subscriber = producer.subscribe();
		} finally {
			spy.mockRestore();
		}

		const group = await subscriber.recvGroup();
		expect(await group?.readString()).toBe("last");
		expect(await subscriber.recvGroup()).toBeUndefined();
		subscriber.close();

		// A closed track protects nothing, so the next scan past the window reclaims it all.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe().close();
		expect(added.filter(([map, key]) => typeof key === "object" && map.has(key)).length).toBe(0);
	} finally {
		clock.installed.close();
	}
});

test("an abort keeps finished groups for a slow reader, then reports it", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(10_000) });
	const arrival = producer.subscribe({ delay: { max: Duration.millis(10_000) } });
	const ordered = producer.subscribe({ delay: { max: Duration.millis(10_000) } }).ordered();
	for (let i = 0; i < 2; i++) {
		const group = hooks.appendNext(producer);
		group.writeFrame(textFrame(`g${i}`, Timestamp.fromMillis(Number(group.start))));
		group.finish();
	}
	hooks.appendNext(producer).writeFrame(textFrame("open", Timestamp.fromMillis(2)));
	const boom = new Error("boom");
	producer.abort(boom);

	for (const next of [() => arrival.recvGroup(), () => ordered.nextGroup()]) {
		for (let i = 0; i < 2; i++) {
			const group = await next();
			expect(group?.start).toBe(BigInt(i));
			expect(await group?.readString()).toBe(`g${i}`);
		}
		// The open group nobody will finish is not handed out.
		await expect(next()).rejects.toBe(boom);
	}
});

test("an abort after the declared end settles ends clean", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(10_000) });
	const arrival = producer.subscribe({ delay: { max: Duration.millis(10_000) } });
	const ordered = producer.subscribe({ delay: { max: Duration.millis(10_000) } }).ordered();
	for (let i = 0; i < 2; i++) {
		const group = hooks.appendNext(producer);
		group.writeFrame(textFrame(`g${i}`, Timestamp.fromMillis(Number(group.start))));
		group.finish();
	}
	producer.finishAt({ group: 2n, frame: 0 });
	producer.abort(new Error("boom"));

	for (const next of [() => arrival.recvGroup(), () => ordered.nextGroup()]) {
		expect((await next())?.start).toBe(0n);
		expect((await next())?.start).toBe(1n);
		expect(await next()).toBeUndefined();
	}
});

test("a settled end survives a late group and an abort", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	const last = producer.createGroup(5n);
	last.finish();
	producer.finishAt({ group: 6n, frame: 0 });
	await settle();
	expect(producer.closed.peek()).toBeNull();

	producer.createGroup(4n);
	producer.abort(new Error("boom"));
	expect(producer.closed.peek()).toBeNull();
	expect((await subscriber.recvGroup())?.start).toBe(5n);
	// The abort cut nothing the end promised, so the subscriber ends cleanly.
	expect(await subscriber.recvGroup()).toBeUndefined();
});

// As in Rust, an evicted group no longer holds the end open.
test("closed settles once the cache evicts the last open group below the end", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
		const open = hooks.appendNext(producer);
		open.writeFrame(textFrame("open", Timestamp.fromMillis(0)));
		const last = hooks.appendNext(producer);
		last.writeFrame(textFrame("last", Timestamp.fromMillis(1)));
		last.finish();
		producer.finish();
		await settle();
		expect(producer.closed.peek()).toBeUndefined();

		await clock.advance(10_000 + CACHE_WINDOW_MS + SWEEP_LAG_MS);
		await settle();
		expect(open.closed.peek()).toBeInstanceOf(TooFarBehind);
		expect(producer.closed.peek()).toBeNull();
	} finally {
		clock.installed.close();
	}
});

test("finish is not terminal: closed settles once the groups below the end finish", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	const group = hooks.appendNext(producer);
	group.writeFrame(textFrame("late", Timestamp.fromMillis(Number(group.start))));
	producer.finish();
	expect(producer.closed.peek()).toBeUndefined();

	// The group below the end is still writable after finish.
	group.writeFrame(textFrame("later", Timestamp.fromMillis(Number(group.start))));
	group.finish();
	await settle();
	expect(producer.closed.peek()).toBeNull();
	// Settled, not released: the producer keeps serving its cache.
	expect(hooks.trackReleased(producer).peek()).toBeUndefined();

	const read = await subscriber.recvGroup();
	expect(read?.start).toBe(0n);
	expect(await subscriber.recvGroup()).toBeUndefined();
	producer.close();
	expect(hooks.trackReleased(producer).peek()).toBeNull();
});

test("closed reports an abort before the end settles", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	hooks.appendNext(producer).writeFrame(textFrame("open"));
	producer.finish();
	const abort = new Error("boom");
	producer.abort(abort);
	expect(producer.closed.peek()).toBe(abort);
});

test("close without finish aborts with a cancel", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.writeFrame(textFrame("only"));
	producer.close();
	const closed = producer.closed.peek();
	expect(closed).toBeInstanceOf(StreamError);
	expect((closed as StreamError).code).toBe(StreamCode.Cancel);
});

test("an ended track's buffered groups age out for a stale subscriber", async () => {
	for (const abort of [undefined, new Error("boom")]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(30) });
		try {
			const stale = producer.subscribe({ delay: { max: Duration.millis(30) } });
			const group = hooks.appendNext(producer);
			group.writeFrame(textFrame("x", Timestamp.fromMillis(Number(group.start))));
			group.finish();
			if (abort) producer.abort(abort);
			else {
				producer.finish();
				producer.close();
			}

			// Nothing writes after close, so the pool's sweep reclaims it.
			await clock.advance(10_000 + CACHE_WINDOW_MS + SWEEP_LAG_MS);
			if (abort) expect(() => stale.tryRecvGroup()).toThrow(abort);
			else expect(stale.tryRecvGroup()).toBeUndefined();
		} finally {
			producer.finish();
			producer.close();
			clock.installed.close();
		}
	}
});

test("an abort leaves a group already taken readable, then reports the abort", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(10_000) });
	const arrival = producer.subscribe({ delay: { max: Duration.millis(10_000) } });
	const ordered = producer.subscribe({ delay: { max: Duration.millis(10_000) } }).ordered();
	hooks.appendNext(producer).writeFrame(textFrame("held"));
	const held = [await arrival.recvGroup(), await ordered.nextGroup()];
	const boom = new Error("boom");
	producer.abort(boom);

	for (const group of held) {
		expect(group?.start).toBe(0n);
		expect(await group?.readString()).toBe("held");
		await expect(group?.readFrame()).rejects.toBe(boom);
	}
});

/** Every group a subscriber can take right now, by sequence. */
function drain(track: { tryRecvGroup(): GroupConsumer | undefined }): bigint[] {
	const keys: bigint[] = [];
	for (let group = track.tryRecvGroup(); group; group = track.tryRecvGroup()) keys.push(group.start);
	return keys;
}

/** A scale-less track holding `count` single-frame groups. */
function untimedTrack(count: number): TrackProducer {
	const producer = new TrackProducer("test").accept({ maxAge: null });
	for (let i = 0; i < count; i++)
		producer.writeFrame({ payload: enc.encode(`${i}`), timestamp: Timestamp.unscaled(i) });
	return producer;
}

test("a frame whose timedness disagrees with its track is refused", async () => {
	const timed = new TrackProducer("timed").accept({ timescale: Timescale.MILLI });
	const group = hooks.appendNext(timed);
	expect(() => group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.unscaled(group.start) })).toThrow(
		TimestampMismatch,
	);
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) });

	const untimed = new TrackProducer("untimed").accept({});
	const subscriber = untimed.subscribe();
	const other = hooks.appendNext(untimed);
	expect(() => other.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) })).toThrow(
		TimestampMismatch,
	);
	other.writeFrame({ payload: enc.encode("y"), timestamp: Timestamp.unscaled(other.start) });
	other.finish();

	// The scale-less frame reaches the subscriber as it was written: its key, no time filled in.
	const frame = await (await subscriber.recvGroup())?.readFrame();
	expect(frame && dec.decode(frame.payload)).toBe("y");
	expect(frame?.timestamp).toEqual(Timestamp.unscaled(other.start));
});

test("unknown track metadata keeps presentation-time expiry until accepted", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("pending");
	try {
		// Appended at the next keys: a timed start has no key before the track's timescale is known.
		for (let second = 0; second < 4; second++) appendAt(producer, second * 1000);
		// No wall time passes. Until metadata is accepted, buffered timestamps still
		// prove that the two oldest groups are outside this reader's media budget.
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(500) } }))).toEqual([2n, 3n]);
		producer.accept({ timescale: Timescale.MILLI, maxAge: null });
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(500) } }))).toEqual([2n, 3n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// The wall clock judges an untimed track, so a new subscriber starts at the oldest cached
// group it doesn't find stale, the way a timed one does: a group is stale once its
// successor arrived a full budget ago.
test("an untimed subscriber starts at the oldest fresh group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ maxAge: null });
	try {
		// Group N's successor arrives N + 1 seconds in.
		for (let i = 0; i < 5; i++) {
			producer.writeFrame({ payload: enc.encode(`${i}`), timestamp: Timestamp.unscaled(i) });
			clock.set(10_000 + (i + 1) * 1000);
		}

		// Four seconds after group 4 arrived, groups 0 to 2 were superseded 4, 3, and 2
		// seconds ago: past a 1.5s budget. Group 3 was superseded 1s ago.
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(1500) } }))).toEqual([3n, 4n]);
		// No budget takes the newest group alone, which is never stale.
		const live = producer.subscribe();
		expect(drain(live)).toEqual([4n]);
		expect(drain(producer.subscribe({ delay: { max: Duration.millis(60_000) } }))).toEqual([0n, 1n, 2n, 3n, 4n]);

		producer.writeFrame({ payload: enc.encode("5"), timestamp: Timestamp.unscaled(5n) });
		expect(drain(live)).toEqual([5n]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("an untimed subscriber without a start begins at the latest group under its end", () => {
	const producer = untimedTrack(5);
	// As the lite publisher serves a SUBSCRIBE with an end and no start.
	const subscriber = producer.subscribe({ delay: { max: Duration.ZERO }, ranges: [{ end: { time: 2n } }] });
	subscriber.withRange({ end: { excluded: 3n } });
	expect(drain(subscriber)).toEqual([2n]);
});

test("a single-frame write that throws aborts its group", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	// A mismatched timestamp is refused before any group exists.
	expect(() => producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.unscaled(0n) })).toThrow(
		TimestampMismatch,
	);
	expect(producer.largest()).toBeUndefined();
	// Group 0, presenting at 10 ms.
	appendAt(producer, 10, "y");
	// A start past group 0's key but before its first frame is only known to rewind once the
	// group exists, so the write ends it.
	expect(() => producer.writeFrame({ payload: enc.encode("z"), timestamp: Timestamp.fromMillis(5) })).toThrow(
		TimestampMismatch,
	);
	// The failed group ends in that error instead of leaving a reader waiting on it.
	expect(subscriber.tryRecvGroup()?.start).toBe(0n);
	const failed = subscriber.tryRecvGroup();
	expect(failed?.start).toBe(5n);
	expect(failed?.closed.peek()).toBeInstanceOf(TimestampMismatch);
});

test("a scale-less single-frame write lands at its key", () => {
	const producer = new TrackProducer("test").accept({ maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	producer.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(3n) });
	// A key at or below one held is a rewind, refused before any group exists.
	expect(() => producer.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(3n) })).toThrow(
		TimestampMismatch,
	);
	expect(() => producer.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(1n) })).toThrow(
		TimestampMismatch,
	);
	producer.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.unscaled(5n) });
	expect(drain(subscriber)).toEqual([3n, 5n]);
	// The keys below each write are skipped, so creating one is refused.
	expect(() => producer.createGroup(4n)).toThrow(Skipped);
});

// An explicit start is a floor, not a request: the budget still skips what it finds stale
// above it, as on a timed track.
test("an explicit start floors an untimed track", () => {
	const producer = untimedTrack(5);

	const resumed = producer.subscribe({
		delay: { max: Duration.millis(60_000) },
		ranges: [{ start: { time: 2n } }],
	});
	expect(drain(resumed)).toEqual([2n, 3n, 4n]);
	const live = producer.subscribe({ delay: { max: Duration.ZERO }, ranges: [{ start: { time: 2n } }] });
	expect(drain(live)).toEqual([4n]);
});

// The error an upstream SUBSCRIBE_DROP gave groups up with.
const DROPPED = new StreamError(StreamCode.NotFound, { message: "dropped" });

test("explicit and producer-cut drops survive outside the group cache", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.dropTrackGroups(producer, 3n, 4n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 3n, end: 4n, error: DROPPED });

	const group = producer.createGroup(4n);
	const cut = new Error("cut");
	group.abort(cut);
	await settle();
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 4n, end: 5n, error: cut });

	reader.close();
	producer.finish();
	producer.close();
});

test("a group ended by a cancel or cache policy is a hole, not a drop", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	const holes = [
		new StreamError(StreamCode.Cancel, { message: "cancel" }),
		new StreamError(StreamCode.Old, { message: "old" }),
		new StreamError(StreamCode.Evicted, { message: "evicted" }),
		new StreamError(StreamCode.DeliveryTimeout, { message: "late" }),
		new TooFarBehind(),
	];
	for (const [key, hole] of holes.entries()) {
		const group = producer.createGroup(BigInt(key));
		group.abort(hole);
	}
	await settle();
	for (const key of holes.keys()) expect(hooks.givenUp(reader, BigInt(key)).error).toBeUndefined();

	reader.close();
	producer.finish();
	producer.close();
});

test("an upstream drop ends a group still arriving with its error", async () => {
	const producer = new TrackProducer("video").accept({ maxAge: null });
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	const arriving = producer.createGroup(3n);
	arriving.writeFrame({ payload: enc.encode("3"), timestamp: Timestamp.unscaled(arriving.start) });
	const finished = producer.createGroup(4n);
	finished.writeFrame({ payload: enc.encode("4"), timestamp: Timestamp.unscaled(finished.start) });
	finished.finish();

	const group = await reader.recvGroup();
	expect(group?.start).toBe(3n);
	expect(await group?.readString()).toBe("3");
	const cut = new StreamError(StreamCode(70), { message: "cut" });
	hooks.dropTrackGroups(producer, 3n, 5n, { error: cut });
	// Settled by the drop itself, not a later reset; the finished group stays whole.
	await expect(group?.readString()).rejects.toBe(cut);
	const whole = await reader.recvGroup();
	expect(whole?.start).toBe(4n);
	expect(await whole?.readString()).toBe("4");

	reader.close();
	producer.finish();
	producer.close();
});

// Write a frame past a group's 32 MiB tail limit, shedding every frame before it.
function overflowTail(group: GroupProducer): void {
	group.writeFrame({ payload: new Uint8Array(32 * 1024 * 1024 + 1), timestamp: Timestamp.unscaled(group.start) });
}

// A reader on a received copy that fails waits out a duplicate still held for its group, then
// continues on it from its frame with nothing repeated or missed. A released hold leaves the
// failure, even to a reader only watching `closed`, as does a duplicate starting past the reader's
// frame. One that sheds the reader's frame after taking over evicts it, as any copy does a reader
// behind it. A re-offer that fails too hands over to the next. A reader closed while it waits ends
// cleanly and can be closed again, and one closed with frames still buffered reads them first.
test.each([
	"reoffer",
	"readable",
	"released",
	"observed",
	"shedObserved",
	"unserved",
	"twice",
	"leaving",
	"closed",
	"past",
	"shed",
])("a failed copy continues on its held re-offer (%p)", async (kind) => {
	const producer = new TrackProducer("video").accept({ maxAge: null });
	hooks.receiveTrack(producer);
	const first = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(first.start) });
	const reader = first.consume();
	expect(await reader.readString()).toBe("a");

	const arriving = hooks.arrivingGroup(producer, 3n);
	expect(arriving).toBeDefined();
	const failed = new Error("reset");
	if (kind === "closed") {
		// A frame still buffered when the copy fails stays readable after a close.
		first.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.unscaled(first.start) });
		first.abort(failed);
		reader.close();
		expect(await reader.readString()).toBe("c");
		expect(await reader.readString()).toBeUndefined();
		arriving?.release();
		producer.close();
		return;
	}
	first.abort(failed);
	if (kind === "observed") {
		let closed: Error | null | undefined;
		reader.closed.then((value) => {
			closed = value;
		});
		await settle();
		expect(closed).toBeUndefined();
		arriving?.release();
		await settle();
		expect(closed).toBe(failed);
		// The failure it reported stands: a re-offer arriving later does not take over.
		const late = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
		late.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(late.start) });
		late.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(late.start) });
		await expect(reader.readString()).rejects.toBe(failed);
		producer.close();
		return;
	}
	if (kind === "twice") {
		// The reader reaches neither re-offer before the third copy takes over.
		const second = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
		arriving?.release();
		const held = hooks.arrivingGroup(producer, 3n);
		second.abort(failed);
		await settle();
		expect(reader.closed.peek()).toBeUndefined();
		const third = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
		held?.release();
		third.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(third.start) });
		third.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(third.start) });
		third.finish();
		expect(await reader.readString()).toBe("b");
		expect(await reader.readString()).toBeUndefined();
		expect(await reader.closed).toBeNull();
		producer.close();
		return;
	}
	if (kind === "shedObserved") {
		// A re-offer that took over and then shed the reader's frame settles `closed` then.
		const again = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
		arriving?.release();
		for (const payload of ["a", "b", "c"])
			again.writeFrame({ payload: enc.encode(payload), timestamp: Timestamp.unscaled(again.start) });
		let closed: Error | null | undefined;
		reader.closed.then((value) => {
			closed = value;
		});
		await settle();
		expect(closed).toBeUndefined();
		overflowTail(again);
		await settle();
		expect(closed).toMatchObject({ code: StreamCode.Evicted });
		producer.close();
		return;
	}
	if (kind === "unserved") {
		// A re-offer starting past the reader's frame cannot take over, so `closed` settles once
		// nothing else is held.
		hooks.receiveGroup(producer, { start: 3n, offset: 2 });
		arriving?.release();
		await settle();
		expect(reader.closed.peek()).toBe(failed);
		producer.close();
		return;
	}
	let read: string | Uint8Array | undefined | Error;
	// A readiness poll waits the same way, and adopts the re-offer rather than spinning on the
	// failed copy.
	const pending =
		kind === "readable" ? reader.readable().then(() => reader.tryReadFrame()?.payload) : reader.readString();
	const reading = pending.then(
		(frame) => {
			read = frame;
		},
		(err: Error) => {
			read = err;
		},
	);
	await settle();
	expect(read).toBeUndefined();
	expect(reader.closed.peek()).toBeUndefined();

	if (kind === "leaving") {
		reader.close();
		await reading;
		expect(read).toBeUndefined();
		// Closing is idempotent, even after the wait was cancelled.
		reader.close();
		arriving?.release();
		producer.close();
		return;
	}
	if (kind === "released") {
		arriving?.release();
		await reading;
		expect(read).toBe(failed);
		expect(await reader.closed).toBe(failed);
		producer.close();
		return;
	}
	const again = hooks.receiveGroup(producer, { start: 3n, offset: kind === "past" ? 2 : 0 });
	arriving?.release();
	if (kind === "shed") {
		// The re-offer drops frames 0 and 1 to its tail limit before the reader reaches it.
		again.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(again.start) });
		again.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(again.start) });
		overflowTail(again);
		await reading;
		expect(read).toMatchObject({ code: StreamCode.Evicted });
		producer.close();
		return;
	}
	if (kind === "past") {
		await reading;
		expect(read).toBe(failed);
		producer.close();
		return;
	}
	await settle();
	expect(read).toBeUndefined();
	again.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(again.start) });
	again.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(again.start) });
	again.finish();
	await reading;
	expect(read).toEqual(kind === "readable" ? enc.encode("b") : "b");
	expect(await reader.readString()).toBeUndefined();
	expect(await reader.closed).toBeNull();
	producer.close();
});

// A stalled reader continued across a chain of takeovers, each copy writing one frame, keeps no more
// than the tail: its unread frames count against each successor's, so it ends evicted past it.
test("a chain of takeovers keeps a stalled reader within the tail", async () => {
	const producer = new TrackProducer("video").accept({ maxAge: null });
	hooks.receiveTrack(producer);
	const failed = new Error("reset");
	const payload = new Uint8Array(9 * 1024 * 1024);
	let copy = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
	const reader = copy.consume();
	for (let index = 0; index < 8; index++) {
		copy.writeFrame({ payload, timestamp: Timestamp.unscaled(copy.start) });
		const held = hooks.arrivingGroup(producer, 3n);
		copy.abort(failed);
		copy = hooks.receiveGroup(producer, { start: 3n, offset: index + 1 });
		held?.release();
	}
	copy.finish();
	expect(await reader.closed).toMatchObject({ code: StreamCode.Evicted });
	producer.close();
});

// While a duplicate is held, a failed copy keeps each mirror open and the copy that takes over tees
// into it from the frame it reached: a sink still buffering the group hands it out once, a frame cut
// short is read whole from the successor unless a reader already took part of it, readers at
// different frames each continue from their own, and with no takeover every mirror fails once.
test.each(["buffered", "handed", "partial", "replay", "positions", "once"])(
	"a held copy's mirrors continue on its successor (%p)",
	async (kind) => {
		const producer = new TrackProducer("video").accept({ maxAge: null });
		hooks.receiveTrack(producer);
		const track = producer.subscribe();
		const first = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
		first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(first.start) });
		const arriving = hooks.arrivingGroup(producer, 3n);
		const failed = new Error("reset");
		const takeover = () => {
			const again = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
			arriving?.release();
			for (const payload of ["a", "b", "c"])
				again.writeFrame({ payload: enc.encode(payload), timestamp: Timestamp.unscaled(again.start) });
			again.finish();
		};

		if (kind === "buffered" || kind === "handed") {
			// The sink hands the group out before or after its copy fails.
			const handed = kind === "handed" ? await track.recvGroup() : undefined;
			if (handed) expect(await handed.readString()).toBe("a");
			first.abort(failed);
			takeover();
			const group = handed ?? (await track.recvGroup());
			expect(group?.start).toBe(3n);
			for (const payload of handed ? ["b", "c"] : ["a", "b", "c"])
				expect(await group?.readString()).toBe(payload);
			expect(await group?.readString()).toBeUndefined();
			// The sink held one copy of the group, so it hands out nothing more.
			expect(await settled(track.recvGroup())).toBe(false);
		} else if (kind === "partial") {
			const ahead = first.consume();
			const behind = first.consume();
			expect(await ahead.readString()).toBe("a");
			const cut = first.createFrame({ size: 2, timestamp: Timestamp.unscaled(first.start) });
			cut.write(enc.encode("b"));
			// `ahead` joins the frame as it arrives; `behind` has yet to reach it.
			const joining = ahead.readString();
			await settle();
			first.abort(failed);
			await expect(joining).rejects.toBe(failed);
			takeover();
			for (const payload of ["a", "b", "c"]) expect(await behind.readString()).toBe(payload);
			expect(await behind.readString()).toBeUndefined();
		} else if (kind === "replay") {
			// The successor fails partway through a frame the reader already has: no loss to it, so a
			// third copy held for the group still continues it.
			const reader = first.consume();
			expect(await reader.readString()).toBe("a");
			first.abort(failed);
			const second = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
			arriving?.release();
			const held = hooks.arrivingGroup(producer, 3n);
			second.createFrame({ size: 2, timestamp: Timestamp.unscaled(second.start) }).write(enc.encode("a"));
			second.abort(failed);
			await settle();
			expect(reader.closed.peek()).toBeUndefined();
			const third = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
			held?.release();
			for (const payload of ["a", "b"])
				third.writeFrame({ payload: enc.encode(payload), timestamp: Timestamp.unscaled(third.start) });
			third.finish();
			expect(await reader.readString()).toBe("b");
			expect(await reader.readString()).toBeUndefined();
		} else if (kind === "positions") {
			first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(first.start) });
			const readers = [first.consume(), first.consume(), first.consume()];
			expect(await readers[1].readString()).toBe("a");
			expect(await readers[2].readString()).toBe("a");
			expect(await readers[2].readString()).toBe("b");
			first.abort(failed);
			takeover();
			const rest = [["a", "b", "c"], ["b", "c"], ["c"]];
			for (const [i, reader] of readers.entries()) {
				for (const payload of rest[i]) expect(await reader.readString()).toBe(payload);
				expect(await reader.readString()).toBeUndefined();
				expect(await reader.closed).toBeNull();
			}
		} else {
			const group = await track.recvGroup();
			const reader = first.consume();
			first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(first.start) });
			first.abort(failed);
			await settle();
			expect(group?.closed.peek()).toBeUndefined();
			expect(reader.closed.peek()).toBeUndefined();
			arriving?.release();
			// A released hold ends each mirror with the copy's failure, once, after what it buffered.
			arriving?.release();
			for (const mirror of [group, reader]) {
				expect(await mirror?.readString()).toBe("a");
				expect(await mirror?.readString()).toBe("b");
				await expect(mirror?.readString()).rejects.toBe(failed);
				expect(await mirror?.closed).toBe(failed);
			}
			// A re-offer arriving later serves new readers only.
			const late = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
			late.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.unscaled(late.start) });
			await expect(reader.readString()).rejects.toBe(failed);
			expect(await late.consume().readString()).toBe("a");
		}
		track.close();
		producer.close();
	},
);

// A subscriber's reader of a failed copy keeps its place when a re-offer replaces that copy in the
// track, continuing on it, and its budget still gives up on it while it waits on a held re-offer.
test.each(["buffered", "expires"])("a subscriber's failed copy waits on its re-offer (%p)", async (kind) => {
	const clock = manualTime(0);
	try {
		await subscriberReoffer(kind);
	} finally {
		clock.installed.close();
	}
});

async function subscriberReoffer(kind: string): Promise<void> {
	const producer = new TrackProducer("video").accept({ timescale: Timescale.MILLI, maxAge: null });
	hooks.receiveTrack(producer);
	const track = producer.subscribe({ delay: { max: Duration.millis(500) } });
	const first = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
	first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(1) });
	const reader = await track.recvGroup();
	if (!reader) throw new Error("missing group");
	expect(await reader.readString()).toBe("a");

	const arriving = hooks.arrivingGroup(producer, 3n);
	const failed = new Error("reset");
	first.abort(failed);
	if (kind === "expires") {
		expect(await reader.readString()).toBe("b");
		let read: unknown;
		reader.readString().then(
			(frame) => {
				read = frame;
			},
			(err: unknown) => {
				read = err;
			},
		);
		// Group 4 bounds group 3's reach at 1s, and group 5 puts the edge a second past it.
		for (const [key, millis] of [
			[4n, 1000],
			[5n, 2000],
		] as const) {
			hooks
				.receiveGroup(producer, { start: key, offset: 0 })
				.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(millis) });
		}
		await settle();
		expect(read).toMatchObject({ code: StreamCode.Old });
		arriving?.release();
		producer.close();
		return;
	}
	// The re-offer replaces the failed copy in the track before the reader takes `b`.
	const again = hooks.receiveGroup(producer, { start: 3n, offset: 0 });
	arriving?.release();
	again.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
	again.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(1) });
	again.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.fromMillis(2) });
	again.finish();
	expect(await reader.readString()).toBe("b");
	expect(await reader.readString()).toBe("c");
	expect(await reader.readString()).toBeUndefined();
	producer.close();
}

test("a drop below the window is reported once the window lowers to it", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer
		.subscribe({ delay: { max: Duration.millis(60_000) } })
		.withRange({ start: { included: 5n } });
	hooks.dropTrackGroups(producer, 2n, 3n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.withRange({ start: { included: 0n } });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2n, end: 3n, error: DROPPED });

	reader.close();
	producer.finish();
	producer.close();
});

test("a re-created cut group is served, not dropped", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	const cut = producer.createGroup(4n);
	cut.abort(new Error("cut"));
	await settle();

	producer.createGroup(4n);
	await settle();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.start : recv).toBe(4n);

	reader.close();
	producer.finish();
	producer.close();
});

test("a reported drop still allows a late group, handed out once", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.dropTrackGroups(producer, 2n, 4n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2n, end: 4n, error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });
	producer.createGroup(3n).finish();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.start : recv).toBe(3n);
	if (recv.kind === "group") recv.group.close();
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });
	reader.close();
	producer.finish();
	producer.close();
});

test("a received stale drop settles only the readers it covers", () => {
	const producer = new TrackProducer("video").accept({ maxAge: null });
	const reader = (max: number, start: bigint, end?: bigint) =>
		producer.subscribe({
			delay: { max: Duration.millis(max) },
			ranges: [{ start: { time: start }, end: end === undefined ? undefined : { time: end - 1n } }],
		});
	const covering = reader(500, 0n);
	const narrower = reader(100, 0n);
	const wider = reader(5000, 0n);
	const capped = reader(500, 0n, 4n);
	const above = reader(500, 3n);
	const stale = staleHole(500);
	hooks.dropTrackGroups(producer, 1n, 3n, stale);
	const skip = skipHole();
	hooks.dropTrackGroups(producer, 3n, 4n, skip);
	const dropped = (subscriber: typeof covering) => {
		const runs: [bigint, bigint, boolean][] = [];
		for (let run = hooks.tryDrop(subscriber); run; run = hooks.tryDrop(subscriber))
			runs.push([run.start, run.end, run.skip === true]);
		return runs;
	};
	const settled: [bigint, bigint, boolean][] = [
		[1n, 3n, false],
		[3n, 4n, true],
	];
	expect(dropped(covering)).toEqual(settled);
	// A narrower budget is settled too.
	expect(dropped(narrower)).toEqual(settled);
	// A wider budget waits, a capped range may judge otherwise, and a range above the run lacks it.
	for (const subscriber of [wider, capped, above]) expect(dropped(subscriber)).toEqual([[3n, 4n, true]]);
	for (const subscriber of [covering, narrower, wider, capped, above]) subscriber.close();
	producer.finish();
	producer.close();
});

test("a superseded stale run is unknown again, and its group is handed out", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(500) }, ranges: [{ start: { time: 0n } }] });
	hooks.dropTrackGroups(producer, 1n, 2n, staleHole(500));
	hooks.supersedeStale(producer);
	expect(hooks.tryDrop(reader)).toBeUndefined();
	producer.createGroup(1n).finish();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.start : recv).toBe(1n);
	if (recv.kind === "group") recv.group.close();
	reader.close();
	producer.finish();
	producer.close();
});

test("superseding withdraws a partly reported stale run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(500) }, ranges: [{ start: { time: 0n } }] });
	hooks.dropTrackGroups(producer, 4n, 5n, { error: DROPPED });
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 4n, end: 5n });
	hooks.dropTrackGroups(producer, 3n, 7n, staleHole(500));
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 3n, end: 4n, stale: 500 });
	hooks.supersedeStale(producer);
	// 5 and 6 are unknown again.
	expect(hooks.tryDrop(reader)).toBeUndefined();
	reader.close();
	producer.finish();
	producer.close();
});

test("a wide drop is held and reported as one run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.dropTrackGroups(producer, 2n, 2n ** 40n, { error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2n, end: 2n ** 40n, error: DROPPED });

	// A wider drop reports only what this cursor has not reported yet, with its own error.
	const wider = new StreamError(StreamCode.Old, { message: "old" });
	hooks.dropTrackGroups(producer, 1n, 2n ** 40n + 2n, { error: wider });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 1n, end: 2n, error: wider });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({
		kind: "drop",
		start: 2n ** 40n,
		end: 2n ** 40n + 2n,
		error: wider,
	});
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.close();
	producer.finish();
	producer.close();
});

test("adjacent drops with the same code merge into one run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	// Each DROP decodes to its own error, so runs must merge by code, not identity.
	for (let key = 2n; key < 5n; key++) {
		hooks.dropTrackGroups(producer, key, key + 1n, {
			error: new StreamError(StreamCode.NotFound, { message: "dropped" }),
		});
	}
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "drop" ? [recv.start, recv.end, recv.error] : recv).toEqual([2n, 5n, DROPPED]);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.close();
	producer.finish();
	producer.close();
});

test("a mid-group end survives eviction and a filtered late reader", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("video").accept({});
	try {
		producer.finishAt({ group: 5n, frame: 5 });
		const group = producer.createGroup(5n);
		for (let i = 0; i < 5; i++)
			group.writeFrame({ payload: new Uint8Array([i]), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
		producer.finish();
		producer.close();
		await clock.advance(10_000 + CACHE_WINDOW_MS + 1);
		expect(await producer.subscribe().largest()).toEqual({ group: 5n, frame: 4 });
		// The cap excludes the only group, so nothing is within it.
		const reader = producer.subscribe().withRange({ end: { excluded: 5n } });
		expect(await reader.largest()).toBeUndefined();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 5n, frame: 5 });
		reader.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a wire end repeats or refines but never moves", () => {
	const producer = new TrackProducer("video").accept({});
	// A FETCH_OK names only the boundary; END_OF_TRACK may then name the frame before it.
	hooks.endTrack(producer, { group: 6n, frame: 0 });
	hooks.endTrack(producer, { group: 5n, frame: 5 });
	hooks.endTrack(producer, { group: 5n, frame: 5 });
	expect(hooks.subscriberEnd(producer.subscribe())).toEqual({ group: 5n, frame: 5 });
	for (const moved of [
		{ group: 5n, frame: 4 },
		{ group: 6n, frame: 0 },
		{ group: 5n, frame: 0 },
	]) {
		expect(() => hooks.endTrack(producer, moved)).toThrow(ProtocolViolation);
	}
	producer.finish();
	producer.close();
});

// An end inside a group that already holds a frame at or past it would leave content the
// end says does not exist.
test("a mid-group end refuses to cut existing frames", () => {
	const producer = new TrackProducer("video").accept({});
	const group = producer.createGroup(5n);
	for (let i = 0; i < 6; i++)
		group.writeFrame({ payload: new Uint8Array([i]), timestamp: Timestamp.unscaled(group.start) });
	expect(() => producer.finishAt({ group: 5n, frame: 5 })).toThrow(ProtocolViolation);
	expect(hooks.subscriberEnd(producer.subscribe())).toBeUndefined();
	producer.finishAt({ group: 5n, frame: 6 });
	producer.finish();
	producer.close();
});

// A mid-group end caps its group, whether the group exists yet or not.
test.each([true, false])("a mid-group end caps later writes (declared first: %s)", (early) => {
	const producer = new TrackProducer("video").accept({});
	const end = { group: 5n, frame: 2 };

	if (early) producer.finishAt(end);
	const group = producer.createGroup(5n);
	group.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.unscaled(group.start) });
	if (!early) producer.finishAt(end);
	group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.unscaled(group.start) });
	expect(() =>
		group.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.unscaled(group.start) }),
	).toThrow("past the track's end");
	expect(group.consume().frameCount).toBe(2);
	expect(() => producer.createGroup(6n)).toThrow();
	producer.finish();
	producer.close();
});

// A closed group `sequence` written into `producer` whose one frame starts at `ms`, aborted
// as ingest does when that frame is refused.
function stampGroup(producer: TrackProducer, key: bigint, ms: number): GroupProducer {
	const group = producer.createGroup(key);
	try {
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	} catch (err) {
		group.abort(err as Error);
		throw err;
	}
	group.finish();
	return group;
}

function drainGroups(subscriber: ReturnType<TrackProducer["subscribe"]>): bigint[] {
	const keys: bigint[] = [];
	for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup()) keys.push(group.start);
	return keys;
}

test("a re-created group is handed out once", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Duration.millis(30_000) });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(30_000) } });
	stampGroup(producer, 0n, 0);
	const aborted = producer.createGroup(1n);
	expect(drainGroups(subscriber)).toEqual([0n, 1n]);

	aborted.abort(new Error("aborted"));
	stampGroup(producer, 1n, 1);
	stampGroup(producer, 2n, 2);
	expect(drainGroups(subscriber)).toEqual([2n]);
});

// A gap is remembered until a group above it is past the budget, max delay or max age alike.
// Then every group below that one counts as handed out.
test("a gap is forgotten once a group above it is past the budget", () => {
	const time = manualTime(1_000);
	try {
		const readers = [
			{ info: { timescale: Timescale.MILLI, maxAge: Duration.millis(30_000) }, maxDelay: Duration.millis(10) },
			{ info: { timescale: Timescale.MILLI, maxAge: Duration.millis(10) }, maxDelay: Duration.millis(3_600_000) },
		];
		for (const { info, maxDelay } of readers) {
			const producer = new TrackProducer("test").accept(info);
			const subscriber = producer.subscribe({ delay: { max: maxDelay } });
			// Group 3 never arrives.
			for (const key of [0n, 1n, 2n, 4n, 5n, 6n]) stampGroup(producer, key, Number(key));
			expect(drainGroups(subscriber)).toEqual([0n, 1n, 2n, 4n, 5n, 6n]);
			// Group 4 reaches until group 5 presents at 5ms, so it is past a 10ms budget once the
			// edge presents at 15ms.
			for (let key = 7n; key <= 14n; key++) {
				stampGroup(producer, key, Number(key));
				expect(drainGroups(subscriber)).toEqual([key]);
				expect(hooks.deliveredRuns(subscriber)).toBe(2);
			}
			stampGroup(producer, 15n, 15);
			expect(drainGroups(subscriber)).toEqual([15n]);
			expect(hooks.deliveredRuns(subscriber)).toBe(1);
		}
	} finally {
		time.installed.close();
	}
});

test("an endless track with gaps remembers only those within the budget", () => {
	const time = manualTime(1_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(30_000),
		});
		// Groups present 1ms apart, so the budget spans ten of them.
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(10) } });
		let most = 0;
		for (let key = 0n; key < 3000n; key++) {
			// Every third group never arrives.
			if (key % 3n === 1n) continue;
			stampGroup(producer, key, Number(key));
			expect(drainGroups(subscriber)).toEqual([key]);
			most = Math.max(most, hooks.deliveredRuns(subscriber));
		}
		expect(most).toBe(5);

		// A late unseen group within the budget is still delivered.
		stampGroup(producer, 2995n, 2995);
		expect(drainGroups(subscriber)).toEqual([2995n]);
	} finally {
		time.installed.close();
	}
});

for (const [name, extra] of [
	["unchecked", { unchecked: true }],
	["IETF provenance", { ietf: IetfVersion.DRAFT_22 }],
] as const) {
	test(`${name} survives normalization and skips the rewind check`, () => {
		for (const timescale of [null, Timescale.MILLI]) {
			const info = authoredInfo({ ...extra, timescale });
			expect(info).toMatchObject(extra);
			expect(info.timescale).toBe(timescale);
		}
		const producer = new TrackProducer("rewind").accept({ timescale: Timescale.MILLI, ...extra });
		try {
			stampGroup(producer, 0n, 1000);
			stampGroup(producer, 1n, 500);
			producer.writeFrame({ payload: enc.encode("arrival"), timestamp: Timestamp.fromMillis(250) });
		} finally {
			producer.finish();
			producer.close();
		}
	});
}

// Mirrors the Rust `an_appended_rewind_is_refused`: starts strictly increase, so an equal start
// is refused like a rewind.
test("an appended rewind is refused, and so is an equal start", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	appendAt(producer, 1000, "a");
	const group = hooks.appendNext(producer);
	expect(() => group.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(500) })).toThrow(
		TimestampMismatch,
	);
	expect(() => group.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.fromMillis(1000) })).toThrow(
		TimestampMismatch,
	);
	group.writeFrame({ payload: enc.encode("d"), timestamp: Timestamp.fromMillis(1001) });
});

test("a rewind is refused whichever group lands first", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	stampGroup(producer, 5n, 1000);
	expect(() => stampGroup(producer, 3n, 2000)).toThrow(TimestampMismatch);
	stampGroup(producer, 3n, 500);
	expect(() => stampGroup(producer, 4n, 400)).toThrow(TimestampMismatch);
	expect(() => stampGroup(producer, 4n, 1100)).toThrow(TimestampMismatch);
	stampGroup(producer, 4n, 700);
});

// Subscribers may have seen an aborted group's start, so a replacement at its sequence leaves
// that start as the watermark, and no later group can start before it.
test("a replaced aborted group still bounds later ones", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const first = producer.createGroup(0n);
	first.writeFrame({ payload: enc.encode("1000"), timestamp: Timestamp.fromMillis(1000) });
	first.abort(new Error("aborted"));

	const replacement = producer.createGroup(0n);
	replacement.writeFrame({ payload: enc.encode("500"), timestamp: Timestamp.fromMillis(500) });
	replacement.finish();
	expect(() => stampGroup(producer, 1n, 600)).toThrow(TimestampMismatch);
	// Past the watermark, not at it: starts strictly increase.
	stampGroup(producer, 1n, 1500);
});

test("a rewind across large timescales is refused", () => {
	const m = Number.MAX_SAFE_INTEGER;
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.writeFrame({ payload: enc.encode("a"), timestamp: new Timestamp(m - 1, Timescale(m)) });
	// (m - 2) / (m - 1) is earlier than (m - 1) / m, though both cross-products round alike.
	expect(() =>
		producer.writeFrame({ payload: enc.encode("b"), timestamp: new Timestamp(m - 2, Timescale(m - 1)) }),
	).toThrow(TimestampMismatch);
	// Exact for fractions too: 0.75s is later than 0.5s.
	const fractions = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	fractions.writeFrame({ payload: enc.encode("a"), timestamp: new Timestamp(0.5, Timescale.SECOND) });
	fractions.writeFrame({ payload: enc.encode("b"), timestamp: new Timestamp(750.5, Timescale.MILLI) });
	expect(() =>
		fractions.writeFrame({ payload: enc.encode("c"), timestamp: new Timestamp(1.5, Timescale(4)) }),
	).toThrow(TimestampMismatch);
});

test("a group's first frame is checked against the group below it", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	stampGroup(producer, 1n, 6000);
	const group = producer.createGroup(2n);
	expect(() => group.writeFrame({ payload: enc.encode("early"), timestamp: Timestamp.fromMillis(5000) })).toThrow(
		TimestampMismatch,
	);
});

test("an evicted group still bounds, and a late group below it is unverified", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(60_000),
		});
		for (const [key, ms] of [
			[0n, 1000],
			[1n, 2000],
			[2n, 3000],
		] as const) {
			stampGroup(producer, key, ms);
		}

		// Idle past the cache window, so the next write evicts every group but the new edge.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		const edge = hooks.appendNext(producer);
		expect(() => edge.writeFrame({ payload: enc.encode("rewind"), timestamp: Timestamp.fromMillis(2500) })).toThrow(
			TimestampMismatch,
		);
		// Past the evicted group 2's start, not at it: starts strictly increase.
		edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(3500) });

		// At or below the watermark the lower neighbor may be gone, so the watermark only caps
		// the group and it is accepted without being trusted to time anything.
		const track = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
		expect(() => stampGroup(producer, 1n, 4000)).toThrow(TimestampMismatch);
		stampGroup(producer, 1n, 500).finish();
		edge.finish();
		producer.finish();
		producer.close();

		const timestamps = new Map<bigint, Timestamp | undefined>();
		for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
			timestamps.set(group.start, hooks.groupTimestamp(group));
		}
		expect(timestamps.get(3n)?.value).toBe(3500);
		expect(timestamps.has(1n)).toBe(true);
		expect(timestamps.get(1n)).toBeUndefined();
	} finally {
		clock.installed.close();
	}
});

test("an untimed track takes groups in any order", () => {
	const producer = new TrackProducer("test").accept({});
	for (const key of [5n, 3n, 4n]) {
		const group = producer.createGroup(key);
		group.writeFrame({ payload: enc.encode(`${key}`), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
	}
});

test("replay and backlogs deliver sparse out-of-order arrivals ascending without sorting", () => {
	const producer = new TrackProducer("test").accept({ maxAge: null });
	const all = { ranges: [{ start: { time: 0n } }], delay: { max: null } };
	const write = (key: bigint) => {
		const group = producer.createGroup(key);
		group.writeFrame({ payload: enc.encode(`${key}`), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
	};
	const drain = (reader: ReturnType<typeof producer.subscribe>) => {
		const seen: bigint[] = [];
		for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) {
			const frame = group.tryReadFrame();
			expect(frame && dec.decode(frame.payload)).toBe(`${group.start}`);
			seen.push(group.start);
		}
		return seen;
	};
	const sort = spyOn(Array.prototype, "sort");
	try {
		const live = producer.subscribe(all);
		// Subscribing merges the subscribers' ranges, which sorts them; only arrivals are judged.
		sort.mockClear();
		for (const key of [40n, 10n, 30n, 0n, 20n]) write(key);
		expect(sort).not.toHaveBeenCalled();
		const joined = producer.subscribe(all);
		sort.mockClear();
		// Arrivals below, between and above the joiner's replayed cache.
		for (const key of [25n, 50n, 5n, 35n]) write(key);
		expect(sort).not.toHaveBeenCalled();
		const expected = [0n, 5n, 10n, 20n, 25n, 30n, 35n, 40n, 50n];
		expect(drain(live)).toEqual(expected);
		expect(drain(joined)).toEqual(expected);
	} finally {
		sort.mockRestore();
	}
});

// A received track as a wire subscription leaves it: the feed starts at group 0 and its end is
// pending, so lower groups may still arrive behind a higher one.
function pendingTail(end: bigint, maxDelay = Duration.millis(60_000)) {
	const producer = new TrackProducer("test");
	// A received copy, whose groups arrive in any order.
	hooks.receiveTrack(producer);
	const reader = producer.subscribe({ delay: { max: maxDelay } });
	hooks.startTrack(producer, { start: 0n });
	hooks.endTrack(producer, { group: end, frame: 0 });
	const write = (key: bigint) => {
		const group = producer.createGroup(key);
		group.finish();
	};
	return { producer, reader, write };
}

/** Whether `read` settled once every pending microtask and timer callback ran. */
async function settled(read: Promise<unknown>): Promise<boolean> {
	let done = false;
	read.then(
		() => (done = true),
		() => (done = true),
	);
	await settle();
	return done;
}

// The end arrives after a higher group did, with lower ones still owed: it must not settle.
test("a received end behind a hole settles only once the hole fills", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.receiveTrack(producer);
	hooks.startTrack(producer, { start: 0n });
	const late = producer.createGroup(2n);
	late.finish();
	hooks.endTrack(producer, { group: 3n, frame: 0 });
	await settle();
	expect(producer.closed.peek()).toBeUndefined();

	for (const key of [0n, 1n]) {
		const group = producer.createGroup(key);
		group.finish();
	}
	await settle();
	expect(producer.closed.peek()).toBeNull();
	reader.close();
});

test("a pending tail holds readers at a hole until it fills", async () => {
	const { producer, reader, write } = pendingTail(3n);
	write(2n);
	expect((await reader.recvGroup())?.start).toBe(2n);
	let next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	write(0n);
	expect((await next)?.start).toBe(0n);
	next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	write(1n);
	expect((await next)?.start).toBe(1n);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(reader.closed.peek()).toBeUndefined();
	producer.finish();
	producer.close();
});

test("a pending tail ends readers at the boundary once the track closes", async () => {
	const { producer, reader, write } = pendingTail(2n);
	write(1n);
	expect((await reader.recvGroup())?.start).toBe(1n);
	const next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	producer.finish();
	producer.close();
	expect(await next).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

// A FETCH_OK's end of track on a copy no subscription feeds: nothing is in flight below it, so a
// reader above the groups another reader's floor still lacks ends at the boundary.
test("an end no subscription feeds owes no tail", async () => {
	const producer = new TrackProducer("test");
	const history = producer.subscribe({
		ranges: [{ start: { time: 0n } }],
		delay: { max: Duration.millis(60_000) },
	});
	const group = producer.createGroup(2n);
	group.finish();
	hooks.endTrack(producer, { group: 3n, frame: 0 });
	const live = producer.subscribe({ ranges: [{ start: { time: 2n } }], delay: { max: Duration.millis(60_000) } });
	expect((await live.recvGroup())?.start).toBe(2n);
	const next = live.recvGroup();
	expect(await settled(next)).toBe(true);
	expect(await next).toBeUndefined();
	history.close();
	live.close();
	producer.close();
});

// A group still in flight when the subscription ended may refine the end, but no subscription
// feeds the tail `cacheTrack` cleared anymore.
test("an end refined after the subscription ended owes no tail", async () => {
	const producer = new TrackProducer("test");
	hooks.receiveTrack(producer);
	const history = producer.subscribe({
		ranges: [{ start: { time: 0n } }],
		delay: { max: Duration.millis(60_000) },
	});
	const group = producer.createGroup(2n);
	group.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	hooks.endTrack(producer, { group: 3n, frame: 0 });
	hooks.cacheTrack(producer);
	hooks.endTrack(producer, { group: 2n, frame: 1 });
	const live = producer.subscribe({ ranges: [{ start: { time: 2n } }], delay: { max: Duration.millis(60_000) } });
	expect((await live.recvGroup())?.start).toBe(2n);
	const next = live.recvGroup();
	expect(await settled(next)).toBe(true);
	expect(await next).toBeUndefined();
	history.close();
	live.close();
	producer.close();
});

test("without a declared start, the first arrival stands in for it", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.endTrack(producer, { group: 3n, frame: 0 });
	const group = producer.createGroup(2n);
	group.finish();
	expect((await reader.recvGroup())?.start).toBe(2n);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.finish();
	producer.close();
});

test("a source start follows the current requested floor after an update", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ ranges: [{ start: { time: 5n } }] });
	try {
		hooks.receiveTrack(producer);
		// Its wire starts a time anchor at the group starting there, so each start is a fixed floor.
		hooks.startsAtKeys(producer);
		hooks.startTrack(producer, { start: 6n, requested: 5n });
		expect(hooks.sourceStart(reader)).toBe(6n);
		const lower = producer.subscribe({ ranges: [{ start: { time: 2n } }] });
		try {
			expect(hooks.sourceStart(lower)).toBe(2n);
			expect(hooks.sourceStart(reader)).toBe(5n);
		} finally {
			const restored = producer.subscription.changed();
			lower.close();
			await restored;
		}
		expect(hooks.sourceStart(reader)).toBe(6n);
		const raised = producer.subscription.changed();
		reader.update({ ranges: [{ start: { time: 9n } }] });
		await raised;
		expect(hooks.sourceStart(reader)).toBe(9n);
	} finally {
		reader.close();
		producer.finish();
		producer.close();
	}
});

// SUBSCRIBE_START answers the floor its SUBSCRIBE asked for, so it holds only while the demand
// keeps that floor: a lowered floor owes the groups it newly asks for.
test("a pending tail owes from SUBSCRIBE_START while the demand keeps its floor", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({
		delay: { max: Duration.millis(60_000) },
		ranges: [{ start: { time: 1n } }],
	});
	hooks.receiveTrack(producer);
	hooks.startTrack(producer, { start: 3n, requested: 1n });
	hooks.endTrack(producer, { group: 4n, frame: 0 });
	const group = producer.createGroup(3n);
	group.finish();
	expect((await reader.recvGroup())?.start).toBe(3n);
	expect(await reader.recvGroup()).toBeUndefined();

	const lower = producer.subscribe({
		delay: { max: Duration.millis(60_000) },
		ranges: [{ start: { time: 0n } }],
	});
	expect((await lower.recvGroup())?.start).toBe(3n);
	const next = lower.recvGroup();
	expect(await settled(next)).toBe(false);
	producer.finish();
	producer.close();
	expect(await next).toBeUndefined();
});

// The wire's end can land after the highest group, with lower groups still owed.
test("an end after the highest group holds the reader at the hole", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ delay: { max: Duration.millis(60_000) } });
	hooks.receiveTrack(producer);
	hooks.startTrack(producer, { start: 0n });
	const high = producer.createGroup(2n);
	high.finish();
	expect((await reader.recvGroup())?.start).toBe(2n);
	const next = reader.recvGroup();

	hooks.endTrack(producer, { group: 3n, frame: 0 });
	expect(await settled(next)).toBe(false);
	for (const key of [0n, 1n]) {
		const low = producer.createGroup(key);
		low.finish();
	}
	expect((await next)?.start).toBe(0n);
	expect((await reader.recvGroup())?.start).toBe(1n);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.finish();
	producer.close();
});

// A group the tail still owed was cut off, so readers get the abort rather than a clean end
// without it.
test("an abort errors a pending tail short of a group", async () => {
	const { producer, reader, write } = pendingTail(2n);
	write(1n);
	expect((await reader.recvGroup())?.start).toBe(1n);
	const next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	producer.abort(new Error("dropped"));
	await expect(next).rejects.toThrow("dropped");
});

test("an abort after a full tail ends cleanly", async () => {
	const { producer, reader, write } = pendingTail(2n);
	write(1n);
	write(0n);
	producer.abort(new Error("dropped"));
	expect((await reader.recvGroup())?.start).toBe(0n);
	expect((await reader.recvGroup())?.start).toBe(1n);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

// A gap the grace already gave up on was lost before the abort, not cut off by it, so readers
// end cleanly. Within the grace, the same abort errors them.
for (const [waited, clean] of [
	[500, false],
	[1000, true],
] as const) {
	test(`an abort ${waited}ms after a hole opened ${clean ? "ends" : "errors"} a pending tail`, async () => {
		const clock = manualTime(0);
		try {
			// The subscription's max delay is the grace the hole is waited for.
			const { producer, reader, write } = pendingTail(3n, Duration.millis(1000));
			write(0n);
			write(2n);
			expect((await reader.recvGroup())?.start).toBe(0n);
			expect((await reader.recvGroup())?.start).toBe(2n);
			const next = reader.recvGroup();
			expect(await settled(next)).toBe(false);

			clock.set(waited);
			producer.abort(new Error("dropped"));
			if (clean) expect(await next).toBeUndefined();
			else await expect(next).rejects.toThrow("dropped");
		} finally {
			clock.installed.close();
		}
	});
}

// Judged by what arrived, not by the cache, so an evicted group still counts.
test("an evicted group still fills a pending tail", async () => {
	const clock = manualTime(0);
	try {
		const { producer, reader, write } = pendingTail(3n);
		for (const key of [0n, 1n]) {
			write(key);
			expect((await reader.recvGroup())?.start).toBe(key);
		}
		clock.set(CACHE_WINDOW_MS + 1000);
		write(2n);
		// A late subscriber replays only what the cache still holds.
		expect(producer.subscribe({ delay: { max: Duration.millis(60_000) } }).tryRecvGroup()?.start).toBe(2n);
		expect((await reader.recvGroup())?.start).toBe(2n);
		expect(await reader.recvGroup()).toBeUndefined();
		producer.finish();
		producer.close();
	} finally {
		clock.installed.close();
	}
});

// SUBSCRIBE_DROP says the publisher gave the range up, so the tail owes nothing there: readers
// end at the boundary, and a later abort cut nothing off.
for (const abort of [false, true]) {
	test(`a dropped range ends a pending tail cleanly${abort ? " despite a later abort" : ""}`, async () => {
		const { producer, reader, write } = pendingTail(3n);
		write(2n);
		expect((await reader.recvGroup())?.start).toBe(2n);
		const next = reader.recvGroup();
		expect(await settled(next)).toBe(false);

		hooks.dropTrackGroups(producer, 0n, 2n, { error: DROPPED });
		if (abort) producer.abort(new Error("dropped"));
		expect(await next).toBeUndefined();
		if (abort) expect(await reader.closed).toBeNull();
		else expect(reader.closed.peek()).toBeUndefined();
		producer.finish();
		producer.close();
	});
}

// A widened budget re-offers the cached groups the reader skipped as stale, and never one it
// already handed out.
test("a widened maxDelay re-offers what it skipped as stale", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
	stampGroup(producer, 0n, 0);
	expect(drainGroups(subscriber)).toEqual([0n]);
	for (const [key, ms] of [
		[1n, 1000],
		[2n, 2000],
		[3n, 3000],
		[4n, 6000],
	] as const) {
		stampGroup(producer, key, ms);
	}
	// Groups 1 and 2 reach 2s and 3s, at least 1.5s behind the 6s edge.
	expect(drainGroups(subscriber)).toEqual([3n, 4n]);

	subscriber.update({ delay: { max: Duration.millis(10_000) } });
	expect(drainGroups(subscriber)).toEqual([1n, 2n]);
	expect(drainGroups(subscriber)).toEqual([]);
	producer.close();
});

// A read already parked when the budget widens wakes for the re-offer.
test("a widened maxDelay wakes a parked read", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
	stampGroup(producer, 0n, 0);
	expect(drainGroups(subscriber)).toEqual([0n]);
	for (const [key, ms] of [
		[1n, 1000],
		[2n, 2000],
		[3n, 6000],
	] as const) {
		stampGroup(producer, key, ms);
	}
	// Group 1 reaches 2s, 4s behind the 6s edge.
	expect(drainGroups(subscriber)).toEqual([2n, 3n]);

	const parked = subscriber.recvGroup();
	subscriber.update({ delay: { max: Duration.millis(10_000) } });
	expect((await parked)?.start).toBe(1n);
	producer.close();
});

// A skip whose group the cache evicted can never be offered again, so it stops holding the
// delivered gaps above it.
test("an evicted stale skip lets the delivered gaps fold", () => {
	const time = manualTime(1_000);
	try {
		const producer = new TrackProducer("test").accept({
			timescale: Timescale.MILLI,
			maxAge: Duration.millis(30_000),
		});
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(10) } });
		stampGroup(producer, 0n, 0);
		expect(drainGroups(subscriber)).toEqual([0n]);
		// Group 1 reaches 5ms, past the 10ms budget behind the 30ms edge.
		stampGroup(producer, 1n, 1);
		stampGroup(producer, 2n, 5);
		stampGroup(producer, 3n, 30);
		expect(drainGroups(subscriber)).toEqual([2n, 3n]);

		// Past the cache window, so the next write evicts group 1.
		time.set(1_000 + 2 * CACHE_WINDOW_MS);
		let most = 0;
		for (let key = 4n; key < 300n; key++) {
			// Every third group never arrives.
			if (key % 3n === 1n) continue;
			stampGroup(producer, key, Number(key) + 30);
			expect(drainGroups(subscriber)).toEqual([key]);
			most = Math.max(most, hooks.deliveredRuns(subscriber));
		}
		expect(most).toBeLessThan(10);
	} finally {
		time.installed.close();
	}
});

// A re-offer never hands out a group below a floor raised in the same update.
test("a widened maxDelay re-offers nothing below a raised floor", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
	stampGroup(producer, 0n, 0);
	expect(drainGroups(subscriber)).toEqual([0n]);
	for (const [key, ms] of [
		[1n, 1000],
		[2n, 2000],
		[3n, 3000],
		[4n, 6000],
	] as const) {
		stampGroup(producer, key, ms);
	}
	expect(drainGroups(subscriber)).toEqual([3n, 4n]);

	subscriber.update({ delay: { max: Duration.millis(10_000) }, ranges: [{ start: { time: 2n } }] });
	expect(drainGroups(subscriber)).toEqual([2n]);
	producer.close();
});

// A stale skip a drop-reading cursor has not reported yet is withdrawn when a widening re-offers
// its group, so the reader never gets both the group and its drop.
for (const reported of [false, true]) {
	test(`a widened maxDelay re-offers a stale group after its drop was ${reported ? "reported" : "pending"}`, () => {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
		const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
		expect(hooks.tryDrop(subscriber)).toBeUndefined();
		stampGroup(producer, 0n, 0);
		expect(drainGroups(subscriber)).toEqual([0n]);
		for (const [key, ms] of [
			[1n, 1000],
			[2n, 2000],
			[3n, 6000],
		] as const) {
			stampGroup(producer, key, ms);
		}
		// Group 1 reaches 2s, 4s behind the 6s edge.
		expect(drainGroups(subscriber)).toEqual([2n, 3n]);

		if (reported) {
			expect(hooks.tryDrop(subscriber)).toMatchObject({ start: 1n, end: 2n });

			expect(hooks.tryDrop(subscriber)).toBeUndefined();
		}
		subscriber.update({ delay: { max: Duration.millis(10_000) } });
		expect(drainGroups(subscriber)).toEqual([1n]);
		expect(hooks.tryDrop(subscriber)).toBeUndefined();
		producer.close();
	});
}

// The lite publisher unreports a stale skip below an idle SUBSCRIBE_START, which never went out, so a
// widening still offers its cached group.
test("an unreported stale skip below an idle start is left to a widening", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
	expect(hooks.tryDrop(subscriber)).toBeUndefined();
	stampGroup(producer, 0n, 0);
	stampGroup(producer, 1n, 1000);
	stampGroup(producer, 2n, 6000);
	// Groups 0 and 1 reach 1s and 6s; only group 0 is 1.5s behind the 6s edge.
	expect(drainGroups(subscriber)).toEqual([1n, 2n]);
	expect(hooks.tryDrop(subscriber)).toMatchObject({ start: 0n, end: 1n });

	// SUBSCRIBE_START resolved at group 1 with the skip still held for it.
	hooks.wire(subscriber).untell(0n, 1n);
	expect(hooks.tryDrop(subscriber)).toBeUndefined();

	subscriber.update({ delay: { max: Duration.millis(10_000) } });
	expect(drainGroups(subscriber)).toEqual([0n]);
	expect(hooks.tryDrop(subscriber)).toBeUndefined();
	producer.close();
});

// A widening that covers only the newer of two skipped groups hands it out without folding the
// older one into the delivered floor: a further widening still re-offers it.
test("a partial maxDelay widening keeps the older skip owed", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const subscriber = producer.subscribe({ delay: { max: Duration.millis(1500) } });
	stampGroup(producer, 0n, 0);
	expect(drainGroups(subscriber)).toEqual([0n]);
	for (const [key, ms] of [
		[1n, 1000],
		[2n, 2000],
		[3n, 4000],
	] as const) {
		stampGroup(producer, key, ms);
	}
	expect(drainGroups(subscriber)).toEqual([2n, 3n]);
	for (const [key, ms] of [
		[4n, 5000],
		[5n, 6000],
		[6n, 9000],
	] as const) {
		stampGroup(producer, key, ms);
	}
	expect(drainGroups(subscriber)).toEqual([5n, 6n]);

	// Group 4 reaches 6s, 3s behind the 9s edge; group 1 reaches 2s, 7s behind.
	subscriber.update({ delay: { max: Duration.millis(4000) } });
	expect(drainGroups(subscriber)).toEqual([4n]);
	subscriber.update({ delay: { max: Duration.millis(10_000) } });
	expect(drainGroups(subscriber)).toEqual([1n]);
	producer.close();
});

for (const ratio of [undefined, 0]) {
	test(`reading a shed front requests refresh only when opted in (${ratio})`, async () => {
		const producer = new TrackProducer("test").accept({ refresh: { ratio } });
		const subscriber = producer.subscribe();
		const ordered = subscriber.ordered();
		const group = hooks.appendNext(producer);
		const consumer = await ordered.nextGroup();
		if (!consumer) throw new Error("missing group");
		for (let index = 0; index < 40; index++)
			group.writeFrame({ payload: new Uint8Array(1024 * 1024), timestamp: Timestamp.unscaled(group.start) });
		await expect(consumer.readFrame()).rejects.toMatchObject({ code: StreamCode.Evicted });
		expect(asked(producer)).toBe(ratio === undefined ? undefined : 1n);
		const joiner = producer.subscribe();
		expect(asked(producer)).toBe(ratio === undefined ? undefined : 1n);

		joiner.close();
		subscriber.close();
		producer.finish();
		producer.close();
	});
}

// A first frame written in chunks sheds its own head without advancing the group's start.
test("reading a shed frame head requests refresh", async () => {
	const producer = new TrackProducer("test").accept({ refresh: { ratio: 0 } });
	const subscriber = producer.subscribe();
	const ordered = subscriber.ordered();
	const group = hooks.appendNext(producer);
	const consumer = await ordered.nextGroup();
	if (!consumer) throw new Error("missing group");
	const MiB = 1024 * 1024;
	const frame = group.createFrame({ size: 40 * MiB, timestamp: Timestamp.unscaled(group.start) });
	for (let index = 0; index < 40; index++) frame.write(new Uint8Array(MiB));
	frame.finish();
	await expect(consumer.readFrame()).rejects.toMatchObject({ code: StreamCode.Evicted });
	expect(asked(producer)).toBe(1n);
	subscriber.close();
	producer.finish();
	producer.close();
});

// A track ended by `fail` keeps the history below its end: a reader gets every group, then the
// error, even one arriving after the producer closed.
test("a failed track keeps its history", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	for (const key of [0n, 1n]) {
		const group = producer.createGroup(key);
		group.writeFrame(textFrame("a", Timestamp.fromMillis(Number(key))));

		group.finish();
	}
	const failed = new StreamError(StreamCode(71));
	producer.fail(failed);
	producer.close();
	expect(producer.closed.peek()).toBe(failed);

	const replay = { ranges: [{ start: { time: 0n } }], delay: { max: Duration.millis(30_000) } };
	const arrival = producer.subscribe(replay);
	expect((await arrival.recvGroup())?.start).toBe(0n);
	expect((await arrival.recvGroup())?.start).toBe(1n);
	await expect(arrival.recvGroup()).rejects.toBe(failed);

	const ordered = producer.subscribe(replay).ordered();
	expect((await ordered.nextGroup())?.start).toBe(0n);
	expect((await ordered.nextGroup())?.start).toBe(1n);
	await expect(ordered.nextGroup()).rejects.toBe(failed);

	// The nonblocking reads end the same way.
	const polled = producer.subscribe(replay);
	expect(polled.tryRecvGroup()?.start).toBe(0n);
	expect(polled.tryRecvGroup()?.start).toBe(1n);
	expect(() => polled.tryRecvGroup()).toThrow(failed);
	const polledOrdered = producer.subscribe(replay).ordered();
	expect(polledOrdered.tryNextGroup()?.start).toBe(0n);
	expect(polledOrdered.tryNextGroup()?.start).toBe(1n);
	expect(() => polledOrdered.tryNextGroup()).toThrow(failed);
});

// `fail` ends where `finish` would, after the highest group written, so a late group below it is
// still taken. Like `finish`, it leaves whatever outcome was already given.
test("fail ends at the live edge and leaves an end already given", async () => {
	const failed = new StreamError(StreamCode(71));
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const latest = producer.createGroup(2n);
	latest.writeFrame(textFrame("c", Timestamp.fromMillis(2)));
	latest.finish();
	producer.fail(failed);
	expect(() => producer.createGroup(3n)).toThrow();
	const late = producer.createGroup(1n);
	late.writeFrame(textFrame("b", Timestamp.fromMillis(1)));
	late.finish();
	const sub = producer.subscribe({ ranges: [{ start: { time: 1n } }], delay: { max: Duration.millis(30_000) } });
	expect((await sub.recvGroup())?.start).toBe(1n);
	expect((await sub.recvGroup())?.start).toBe(2n);
	await expect(sub.recvGroup()).rejects.toBe(failed);
	producer.close();

	// An end already declared stands: the track still finishes cleanly.
	const finished = new TrackProducer("finished").accept({ timescale: Timescale.MILLI, maxAge: null });
	finished.finish();
	finished.fail(failed);
	finished.close();
	expect(finished.closed.peek()).toBeNull();

	// So does an abort.
	const aborted = new TrackProducer("aborted").accept({ timescale: Timescale.MILLI, maxAge: null });
	const cause = new StreamError(StreamCode(70));
	aborted.abort(cause);
	aborted.fail(failed);
	expect(aborted.closed.peek()).toBe(cause);
});

// Creating a group says nothing about the gap it leaves: groups may be created out of order. An
// append says the producer will not go back, so it skips every hole below it that was never
// produced, replacing a drop already given and reported. A received copy's groups arrive in any
// order, so its gap is not skipped. Mirrors Rust.
test("an append skips the holes created groups left below it", () => {
	const producer = new TrackProducer("test");
	producer.accept({ maxAge: null });
	const reader = producer.subscribe({ delay: { max: null } });
	const drain = () => {
		const got: unknown[] = [];
		for (
			let next = hooks.tryRecvGroup(reader, true);
			next.kind !== "idle";
			next = hooks.tryRecvGroup(reader, true)
		) {
			if (next.kind === "group") got.push(next.group.start);
			else if (next.kind === "drop") got.push([next.start, next.end, next.skip === true]);
			else throw new Error(`unexpected ${next.kind}`);
		}
		return got;
	};
	producer.createGroup(0n).finish();
	hooks.dropTrackGroups(producer, 2n, 3n, { error: DROPPED });
	producer.createGroup(4n).finish();
	expect(drain()).toEqual([[2n, 3n, false], 0n, 4n]);

	hooks.appendNext(producer).finish();
	expect(drain()).toEqual([[1n, 4n, true], 5n]);

	// The next append only names what was left since this one.
	producer.createGroup(7n).finish();
	hooks.appendNext(producer).finish();
	expect(drain()).toEqual([[6n, 7n, true], 7n, 8n]);

	const copy = new TrackProducer("test");
	hooks.receiveTrack(copy);
	copy.accept({ maxAge: null });
	const late = copy.subscribe({ delay: { max: null } });
	copy.createGroup(0n).finish();
	copy.createGroup(2n).finish();
	hooks.appendNext(copy).finish();
	expect(hooks.tryRecvGroup(late, true)).toMatchObject({ kind: "group" });
	expect(hooks.tryRecvGroup(late, true)).toMatchObject({ kind: "group" });
	expect(hooks.tryRecvGroup(late, true)).toMatchObject({ kind: "group" });
	expect(hooks.tryRecvGroup(late, true)).toEqual({ kind: "idle" });
});

// A late group created out of order reaches a reader, since creating a group skips nothing.
test("a late local group reaches a reader, since creating out of order skips nothing", () => {
	const producer = new TrackProducer("test");
	producer.accept({ maxAge: null });
	producer.createGroup(0n).finish();
	producer.createGroup(2n).finish();
	const reader = producer.subscribe({ delay: { max: null }, order: "ascending" });
	producer.createGroup(1n).finish();
	const got: bigint[] = [];
	for (let group = reader.tryRecvGroup(); group; group = reader.tryRecvGroup()) got.push(group.start);
	expect(got).toEqual([0n, 1n, 2n]);
});

// Declared skips up to the end settle it, as written groups would: an open-range reader ends after
// the groups below, and the track's outcome resolves. Mirrors the Rust `TrackState::reaches`.
test("declared skips up to the end settle the track", async () => {
	const producer = new TrackProducer("test");
	producer.accept({});
	const reader = producer.subscribe({ delay: { max: null }, order: "ascending" });
	producer.createGroup(0n).finish();
	// Group 0's finish settles first, so only the skip can reach the end.
	await yieldNow();
	producer.finishAt({ group: 3n, frame: 0 });
	expect(producer.closed.peek()).toBeUndefined();
	producer.skipGroups(1n, 3n);
	expect(producer.closed.peek()).toBeNull();
	expect((await reader.recvGroup())?.start).toBe(0n);
	expect(await reader.recvGroup()).toBeUndefined();
	reader.close();
});

// A skip is final: creating at a sequence an append or skipGroups skipped throws, and a later drop
// leaves it skipped, while a dropped sequence may be created again. skipGroups leaves cached
// sequences alone and replaces every drop, final or stale. Mirrors Rust.
test("creating a skipped sequence throws Skipped", () => {
	const producer = new TrackProducer("test");
	producer.accept({});
	producer.createGroup(0n).finish();
	producer.createGroup(3n).finish();
	hooks.appendNext(producer).finish();
	expect(() => producer.createGroup(1n)).toThrow(Skipped);

	producer.skipGroups(5n, 7n);
	expect(() => producer.createGroup(6n)).toThrow(Skipped);
	// No skip below group 0.
	expect(() => producer.skipGroups(-1n, 1n)).toThrow(RangeError);
	// The next sequence, 5, was skipped too.
	expect(() => hooks.appendNext(producer)).toThrow(Skipped);
	// A later drop leaves a skipped sequence skipped.
	expect(hooks.dropTrackGroups(producer, 1n, 2n, { error: DROPPED })).toEqual([]);
	expect(() => producer.createGroup(1n)).toThrow(Skipped);
	// A dropped sequence may be created again.
	hooks.dropTrackGroups(producer, 9n, 10n, { error: DROPPED });
	producer.createGroup(9n).finish();

	const other = new TrackProducer("test");
	other.accept({});
	const reader = other.subscribe({ delay: { max: null } });
	other.createGroup(1n).finish();
	hooks.dropTrackGroups(other, 3n, 4n, { error: DROPPED });
	hooks.dropTrackGroups(other, 4n, 5n, staleHole(1000));
	other.skipGroups(0n, 5n);
	hooks.supersedeStale(other);
	const drops: unknown[] = [];
	for (let next = hooks.tryRecvGroup(reader, true); next.kind !== "idle"; next = hooks.tryRecvGroup(reader, true)) {
		if (next.kind === "drop") drops.push([next.start, next.end, next.skip === true]);
	}
	expect(drops).toEqual([
		[0n, 1n, true],
		[2n, 5n, true],
	]);
});

// A skip after a drop already reported goes out again, replacing it, while a reader that moved past
// the drop is handed nothing again; a skip already reported is not reported twice, and a later drop
// leaves it skipped. Mirrors Rust.
test("a skip is reported over a reported drop", () => {
	const producer = new TrackProducer("test");
	producer.accept({ maxAge: null });
	const reader = producer.subscribe({ delay: { max: null } });
	const drain = () => {
		const got: unknown[] = [];
		for (
			let next = hooks.tryRecvGroup(reader, true);
			next.kind !== "idle";
			next = hooks.tryRecvGroup(reader, true)
		) {
			if (next.kind === "group") got.push(next.group.start);
			else if (next.kind === "drop") got.push([next.start, next.end, next.skip === true]);
			else throw new Error(`unexpected ${next.kind}`);
		}
		return got;
	};
	producer.createGroup(0n).finish();
	hooks.dropTrackGroups(producer, 1n, 3n, { error: DROPPED });
	hooks.dropTrackGroups(producer, 4n, 5n, { error: DROPPED });
	producer.createGroup(5n).finish();
	expect(drain()).toEqual([[1n, 3n, false], [4n, 5n, false], 0n, 5n]);

	// Group 0 arrived, so the skip leaves it; both drops and the gap between become one skip.
	expect(hooks.skipTrackGroups(producer, 0n, 5n)).toEqual([{ start: 0n, end: 1n }]);
	expect(drain()).toEqual([[1n, 5n, true]]);

	expect(hooks.dropTrackGroups(producer, 2n, 3n, { error: DROPPED })).toEqual([]);
	expect(drain()).toEqual([]);
	expect(() => producer.createGroup(2n)).toThrow(Skipped);
	reader.close();
	producer.close();
});

// Skips are kept and reported as runs: a huge skipGroups or append gap costs one run and one
// report, whatever its length. Mirrors Rust.
test("a huge skip costs one run", () => {
	const FAR = 1n << 61n;
	for (const append of [false, true]) {
		const producer = new TrackProducer("test");
		producer.accept({ maxAge: null });
		const reader = producer.subscribe({ delay: { max: null } });
		producer.createGroup(0n).finish();
		if (append) {
			producer.createGroup(FAR).finish();
			hooks.appendNext(producer).finish();
		} else {
			producer.skipGroups(1n, FAR);
			producer.createGroup(FAR).finish();
		}
		const got: unknown[] = [];
		for (
			let next = hooks.tryRecvGroup(reader, true);
			next.kind !== "idle";
			next = hooks.tryRecvGroup(reader, true)
		) {
			if (next.kind === "group") got.push(next.group.start);
			else if (next.kind === "drop") got.push([next.start, next.end, next.skip === true]);
		}
		const groups = append ? [0n, FAR, FAR + 1n] : [0n, FAR];
		expect(got.filter((entry) => typeof entry === "bigint")).toEqual(groups);
		expect(got.filter((entry) => typeof entry !== "bigint")).toEqual([[1n, FAR, true]]);
	}
});

// Mirrors moq-net's `the_reserved_sequence_is_refused` and `a_group_at_the_reserved_sequence_is_refused`.
test("the reserved sequence is refused", () => {
	const reserved = (1n << 64n) - 1n;
	const producer = new TrackProducer("test").accept({});
	expect(() => producer.createGroup(1n << 64n)).toThrow(RangeError);
	expect(() => producer.createGroup(reserved)).toThrow(RangeError);
	expect(() => hooks.receiveGroup(producer, { start: reserved, offset: 0 })).toThrow(RangeError);
	producer.createGroup(reserved - 1n).finish();
	expect(() => hooks.appendNext(producer)).toThrow(RangeError);
	// A run's exclusive end may sit at the reserved sequence, never past it.
	producer.skipGroups(1n << 63n, reserved);
	expect(() => producer.skipGroups(0n, 1n << 64n)).toThrow(RangeError);
});

// Mirrors the Rust `track::Finished` doctests: the handle `finish` returns takes no new
// writes, at compile time and at run time, yet still fills a late group below the end.
test("finishing returns handles that write nothing new but fill a late group", async () => {
	const producer = new TrackProducer("test");
	const subscriber = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 0n } }] });
	producer.createGroup(1n).finish();
	const finished = producer.finish();
	// @ts-expect-error a finished track appends no group
	expect(() => finished.appendGroup(Timestamp.unscaled(2n))).toThrow("at or past the track's end");
	expect(() => finished.createGroup(2n)).toThrow("at or past the track's end");

	const late = finished.createGroup(0n);
	late.writeFrame(textFrame("late", Timestamp.fromMillis(Number(late.start))));
	const group = late.finish();
	// @ts-expect-error a finished group takes no frame
	expect(() => group.writeFrame(textFrame("later", Timestamp.fromMillis(Number(group.start))))).toThrow();
	const reader = group.consume();
	expect(dec.decode((await reader.readFrame())?.payload)).toBe("late");
	expect(await reader.readFrame()).toBeUndefined();
	expect(await group.closed).toBeNull();

	const got: bigint[] = [];
	for (let next = subscriber.tryRecvGroup(); next; next = subscriber.tryRecvGroup()) got.push(next.start);
	expect(got.sort()).toEqual([0n, 1n]);
	expect(hooks.tryRecvGroup(subscriber, false)).toEqual({ kind: "done" });
	finished.close();
});

// Mirrors moq-net's `unplaced_holds_only_what_containing_cant_place`: a run holds only times
// `containing` can't place, but those a reported start places directly, and leaves out only
// times below the newest start reported past an earlier range's time.
test("track: a reader's unplaced times are only times its holdings can't place", () => {
	let seed = 5;
	const random = (below: number) => {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		// The high bits: an LCG's low bits cycle.
		return (seed >>> 16) % below;
	};
	const errors = [new StreamError(StreamCode.NotFound), new StreamError(StreamCode.Cancel)];
	for (let round = 0; round < 400; round++) {
		const received = random(2) === 0;
		const producer = received ? receivedTrack() : new TrackProducer("test").accept({ maxAge: null });
		const starts: [bigint, bigint][] = [];
		if (received) {
			for (let n = random(3); n > 0; n--) starts.push([BigInt(random(48)), BigInt(random(48))]);
			hooks.timeStarts(producer, starts);
		}
		const reported = new Set(starts.map(([time]) => time));
		let past = 0n;
		for (const [asked, start] of new Map(starts)) if (asked < start && start > past) past = start;
		const reader = producer.subscribe();
		for (let step = random(16); step >= 0; step--) {
			const holdings = hooks.subscriberHoldings(reader);
			const unplaced = holdings.unplaced?.();
			expect(unplaced).toBeDefined();
			for (let at = 0n; at < 64n; at++) {
				const open = unplaced?.runs.some((run) => run.start <= at && at < run.end) ?? false;
				const unknown = holdings.containing(at) === "unknown";
				if (reported.has(at)) expect([at, unknown]).toEqual([at, false]);
				else if (open) expect([at, unknown]).toEqual([at, true]);
				else if (unknown) expect([at, received && at < past]).toEqual([at, true]);
			}
			const key = BigInt(random(48));
			if (random(2) === 0) {
				try {
					putGroup(producer, key);
				} catch {
					// A key this copy refuses, such as one a skip covers.
				}
			} else hooks.dropTrackGroups(producer, key, key + BigInt(1 + random(4)), { error: errors[random(2)] });
		}
		producer.close();
	}
});

// The aggregate as it was folded from every subscriber before it was counted, kept to check the
// counted one against: Rust's Subscription::poll_combined.
function foldSubscriptions(subscriptions: Iterable<Subscription | undefined>): Subscription | undefined {
	const largest = (a: Duration | null | undefined, b: Duration | null | undefined): Duration | null => {
		if (a === null || a === undefined) return b ?? null;
		if (b === null || b === undefined) return a;
		return a.max(b);
	};
	let combined: Subscription | undefined;
	for (const subscription of subscriptions) {
		if (!subscription) continue;
		if (!combined) {
			combined = { ...subscription };
			continue;
		}
		combined.priority = Math.max(combined.priority ?? 0, subscription.priority ?? 0);
		combined.delay = {
			max: Duration.millis(Math.max(maxDelayOf(combined), maxDelayOf(subscription))),
			min: largest(combined.delay?.min, subscription.delay?.min),
		};
		combined.order =
			combined.order === "ascending" && subscription.order === "ascending" ? "ascending" : "descending";
		combined.ranges = [...(combined.ranges ?? []), ...(subscription.ranges ?? [])];
	}
	if (combined) combined.ranges = union(combined.ranges ?? []);
	return combined;
}

// Joins, whole replacements, edits by key, closes, and fetches each move the counted aggregates
// to exactly what folding every subscriber gives.
test("the counted aggregates match a full fold of every subscriber", async () => {
	let seed = 7;
	const random = (below: number) => {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		// The high bits: an LCG's low bits cycle.
		return (seed >>> 16) % below;
	};
	const anchor = (): Anchor | undefined => {
		const kind = random(6);
		if (kind === 0) return { relative: BigInt(random(4)) };
		if (kind === 1) return undefined;
		return { time: BigInt(random(24)) };
	};
	const range = (): Range => {
		const out: Range = {};
		const start = anchor();
		if (start) out.start = start;
		const end = random(3) === 0 ? undefined : anchor();
		if (end) out.end = end;
		return out;
	};
	const delay = (): Delay => ({
		max: random(5) === 0 ? null : Duration.millis(random(4) * 100),
		min: random(3) === 0 ? null : Duration.millis(random(4) * 50),
	});
	const order = (): Order => (random(2) === 0 ? "ascending" : "descending");
	type Held = {
		subscriber: ReturnType<TrackProducer["subscribe"]>;
		keys: Map<bigint, Range>;
		next: bigint;
		fetch: boolean;
	};
	for (let round = 0; round < 200; round++) {
		const producer = new TrackProducer("test");
		const held: Held[] = [];
		for (let step = 0; step < 60; step++) {
			const op = held.length === 0 ? 0 : random(8);
			const pick = held[random(held.length)];
			if (op === 0 && held.length < 6) {
				const ranges = Array.from({ length: random(6) }, range);
				const subscriber = producer.subscribe({ priority: random(4), order: order(), delay: delay(), ranges });
				held.push({
					subscriber,
					keys: new Map(ranges.map((r, i) => [BigInt(i), r])),
					next: BigInt(ranges.length),
					fetch: false,
				});
			} else if (op === 1 && !pick.fetch) {
				const ranges = Array.from({ length: random(6) }, range);
				const before = [...pick.keys.values()];
				pick.subscriber.update({ priority: random(4), order: order(), delay: delay(), ranges });
				// A whole replacement rekeys by position only when the ranges change.
				if (!Bun.deepEquals(before, ranges)) {
					pick.keys = new Map(ranges.map((r, i) => [BigInt(i), r]));
					if (pick.next < BigInt(ranges.length)) pick.next = BigInt(ranges.length);
				}
			} else if (op <= 4 && !pick.fetch) {
				const retire: bigint[] = [];
				const add: [bigint, Range][] = [];
				for (const key of pick.keys.keys()) if (random(3) === 0) retire.push(key);
				for (const key of pick.keys.keys()) if (random(5) === 0) add.push([key, range()]);
				for (let count = random(3); count > 0; count--) add.push([pick.next++, range()]);
				const readded = new Set(add.map(([key]) => key));
				for (const key of retire) if (!readded.has(key)) pick.keys.delete(key);
				for (const [key, r] of add) pick.keys.set(key, r);
				hooks.editSubscriber(pick.subscriber, {
					retire,
					add,
					priority: random(4),
					order: order(),
					delay: delay(),
				});
			} else if (op === 5 && !pick.fetch) {
				hooks.fetchBudget(pick.subscriber, Duration.millis(random(4) * 100));
				pick.fetch = true;
			} else if (op >= 6) {
				pick.subscriber.close();
				held.splice(held.indexOf(pick), 1);
			}
			// A close leaves the aggregates once its signal fires.
			await yieldNow();
			for (const { subscriber, keys } of held)
				expect(subscriber.subscription.peek()?.ranges).toEqual([...keys.values()]);
			const all = held.map(({ subscriber }) => subscriber.subscription.peek());
			const live = held.filter(({ fetch }) => !fetch).map(({ subscriber }) => subscriber.subscription.peek());
			expect(hooks.requested(producer).peek()).toEqual(foldSubscriptions(all));
			expect(producer.subscription.peek()).toEqual(foldSubscriptions(live));
		}
		producer.close();
	}
});
