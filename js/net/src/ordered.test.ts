// What an ordered read returns from nextItem: groups in key order, and a gap for every group it gives up on.
// Ports the Rust `track::Ordered` item tests, so both read the same.
import { expect, test } from "bun:test";
import { Duration, sleep } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { StreamCode, StreamError } from "./error.ts";
import { hooks, NO_MAX_DELAY, staleHole } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { Timestamp } from "./time.ts";
import { type Ordered, Producer as TrackProducer } from "./track.ts";

// A session-fed copy owes holes; an in-process producer does not.
function receivedTrack() {
	const producer = new TrackProducer("test").accept({ maxAge: null });
	hooks.receiveTrack(producer);
	return producer;
}

// A finished one-frame group, presenting at its key in milliseconds on a timed track.
function put(producer: TrackProducer, key: bigint, timed = true) {
	const group = producer.createGroup(key);
	group.writeFrame(textFrame("x", timed ? Timestamp.fromMillis(Number(key)) : Timestamp.unscaled(key)));
	group.finish();
}

function drop(producer: TrackProducer, start: bigint, end: bigint) {
	hooks.dropTrackGroups(producer, start, end, { error: new StreamError(StreamCode.DeliveryTimeout) });
}

const NAMES: Record<number, string> = {
	[StreamCode.DeliveryTimeout]: "delivery timeout",
	[StreamCode.Old]: "old",
	[StreamCode.NotFound]: "not found",
};

// What an ordered read returns until it would wait, as `group 2` or `gap 1..2 (old)`.
function items(ordered: Ordered): string[] {
	const out: string[] = [];
	for (;;) {
		if (out.length > 20) throw new Error(`no end to the items: ${out.join(", ")}`);
		const item = ordered.tryNextItem();
		if (!item) return out;
		if (item.kind === "group") {
			out.push(`group ${item.group.start}`);
			item.group.close();
		} else {
			const code = item.error instanceof StreamError ? item.error.code : -1;
			out.push(`gap ${item.start}..${item.end} (${NAMES[code] ?? item.error.message})`);
		}
	}
}

function withClock(fn: (clock: Manual) => void) {
	const clock = new Manual();
	const installed = clock.install();
	try {
		fn(clock);
	} finally {
		installed.close();
	}
}

test("an ordered read reports what it gives up, in key order, but no skip", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	put(producer, 3n);
	drop(producer, 1n, 3n);
	producer.skipGroups(4n, 6n);
	put(producer, 6n);
	expect(items(ordered)).toEqual(["group 0", "gap 1..3 (delivery timeout)", "group 3", "group 6"]);

	// A group handed out and then dropped is never reported again.
	const cut = producer.createGroup(7n);
	const read = ordered.tryNextItem();
	expect(read?.group?.start).toBe(7n);
	drop(producer, 7n, 8n);
	cut.abort(new StreamError(StreamCode.Cancel));
	expect(items(ordered)).toEqual([]);
});

test("nextGroup and tryNextGroup pass the gaps nextItem reports", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	drop(producer, 3n, 4n);
	put(producer, 4n);
	expect((await ordered.nextGroup())?.start).toBe(0n);
	expect(ordered.tryNextGroup()?.start).toBe(2n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 3n, end: 4n });
	expect((await ordered.nextGroup())?.start).toBe(4n);
});

test("keys below where a session's feed starts are no gap unless a fill owed them", () => {
	const producer = receivedTrack();
	hooks.startTrack(producer, { start: 10n });
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	// Group 0 comes from history, as a fill would bring it; the feed owes nothing below 10.
	put(producer, 0n, false);
	put(producer, 10n, false);
	expect(items(ordered)).toEqual(["group 0", "group 10"]);
});

test("a group held behind a gap is not returned once the reader closes", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	ordered.close();
	expect(ordered.tryNextItem()).toBeUndefined();
});

test("a key passed under a stale verdict that does not settle this reader is still a gap", () =>
	withClock((clock) => {
		const producer = receivedTrack();
		const ordered = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
		put(producer, 0n, false);
		expect(items(ordered)).toEqual(["group 0"]);
		// The feed gave key 1 up under a 10 ms budget, which binds no reader waiting 50 ms.
		hooks.dropTrackGroups(producer, 1n, 2n, staleHole(10));
		put(producer, 2n, false);
		clock.jump(Duration.millis(60));
		put(producer, 3n, false);
		expect(items(ordered)).toEqual(["gap 1..2 (old)", "group 2", "group 3"]);
	}));

test("a key a local track never produced is no gap", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: Duration.secs(1) } }).ordered();
	put(producer, 0n);
	put(producer, 2n);
	expect(items(ordered)).toEqual(["group 0", "group 2"]);
});

test("a gap waits behind what is owed below it, and goes out before the end", () =>
	withClock(() => {
		const producer = receivedTrack();
		const ordered = producer.subscribe({ delay: { max: null } }).ordered();
		put(producer, 0n, false);
		put(producer, 2n, false);
		drop(producer, 4n, 5n);
		expect(items(ordered)).toEqual(["group 0"]);
		put(producer, 1n, false);
		expect(items(ordered)).toEqual(["group 1", "group 2"]);
		put(producer, 3n, false);
		expect(items(ordered)).toEqual(["group 3", "gap 4..5 (delivery timeout)"]);
	}));

test("the last gap goes out before the end, and the read then ends", async () => {
	const local = new TrackProducer("test");
	const read = local.subscribe({ delay: { max: null } }).ordered();
	put(local, 0n);
	drop(local, 1n, 2n);
	local.finish();
	expect(items(read)).toEqual(["group 0", "gap 1..2 (delivery timeout)"]);
	const end = await Promise.race([read.nextItem(), sleep(Duration.millis(100)).then(() => "hung")]);
	expect(end).toBeUndefined();
});

test("a gap is returned once, even when a later drop names it again", () =>
	withClock((clock) => {
		const producer = receivedTrack();
		const ordered = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
		put(producer, 0n, false);
		put(producer, 2n, false);
		expect(items(ordered)).toEqual(["group 0"]);
		// The budget gives up on the group still owed, as stale.
		clock.jump(Duration.millis(50));
		expect(items(ordered)).toEqual(["gap 1..2 (old)", "group 2"]);
		drop(producer, 1n, 2n);
		put(producer, 3n, false);
		expect(items(ordered)).toEqual(["group 3"]);
	}));

test("a skip replaces a gap still waiting, and the rest of the drop stays one", () =>
	withClock(() => {
		const producer = receivedTrack();
		const ordered = producer.subscribe({ delay: { max: null } }).ordered();
		put(producer, 0n, false);
		put(producer, 2n, false);
		drop(producer, 4n, 8n);
		expect(items(ordered)).toEqual(["group 0"]);
		producer.skipGroups(5n, 7n);
		drop(producer, 1n, 2n);
		expect(items(ordered)).toEqual(["gap 1..2 (delivery timeout)", "group 2"]);
		put(producer, 3n, false);
		expect(items(ordered)).toEqual(["group 3", "gap 4..5 (delivery timeout)", "gap 7..8 (delivery timeout)"]);
	}));

test("a cached group the seek passes as stale is a gap", () =>
	withClock((clock) => {
		const producer = receivedTrack();
		put(producer, 0n, false);
		put(producer, 1n, false);
		clock.jump(Duration.millis(100));
		put(producer, 2n, false);
		const ordered = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
		expect(items(ordered)).toEqual(["gap 0..1 (old)", "group 1", "group 2"]);
	}));

// Rust reports it Old; JS keeps no record below the read, so its memory stays bounded.
test("a local track's late arrival is no gap", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	put(producer, 5n);
	expect(items(ordered)).toEqual(["group 0", "group 5"]);
	put(producer, 3n);
	expect(items(ordered)).toEqual([]);
	put(producer, 6n);
	expect(items(ordered)).toEqual(["group 6"]);
});

test("a gap is clipped to the reader's range", () => {
	const producer = new TrackProducer("test");
	const ordered = producer
		.subscribe({ delay: { max: null } })
		.ordered()
		.withRange({ start: { included: 2n }, end: { excluded: 4n } });
	drop(producer, 0n, 6n);
	put(producer, 5n);
	expect(items(ordered)).toEqual(["gap 2..4 (delivery timeout)"]);
});

test("the frame helpers read past a gap", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	expect((await ordered.readFrame())?.group).toBe(0n);
	expect((await ordered.readFrame())?.group).toBe(2n);
});

test("a local track waits for what it asked a fill for, and reports a gap once the budget gives up", () =>
	withClock((clock) => {
		const producer = new TrackProducer("test");
		const requests = producer.requests();
		const ordered = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
		put(producer, 0n);
		put(producer, 2n);
		expect(items(ordered)).toEqual(["group 0"]);
		// Group 1 lands late, within the budget, as a group does.
		put(producer, 1n);
		expect(items(ordered)).toEqual(["group 1", "group 2"]);
		put(producer, 4n);
		expect(items(ordered)).toEqual([]);
		clock.jump(Duration.millis(50));
		expect(items(ordered)).toEqual(["gap 3..4 (old)", "group 4"]);
		requests.close();
	}));

test("a gap moves the read past it, so a group landing there after is no group", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	expect(items(ordered)).toEqual(["group 0"]);
	drop(producer, 1n, 2n);
	expect(items(ordered)).toEqual(["gap 1..2 (delivery timeout)"]);
	put(producer, 1n);
	expect(items(ordered)).toEqual([]);
	put(producer, 2n);
	expect(items(ordered)).toEqual(["group 2"]);
});

test("a group held past a gap, under a cap lowered off it, still sees the read end", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	expect((await ordered.nextItem())?.group?.start).toBe(0n);
	expect((await ordered.nextItem())?.kind).toBe("gap");
	ordered.withRange({ end: { excluded: 2n } });
	expect(ordered.tryNextItem()).toBeUndefined();
	const pending = ordered.nextItem();
	ordered.close();
	const ended = await Promise.race([
		pending.then(
			(item) => (item === undefined ? "ended" : `item ${item.kind}`),
			(err: Error) => `rejected ${err.message}`,
		),
		sleep(Duration.millis(100)).then(() => "hung"),
	]);
	expect(ended).toBe("ended");
});

test("a stale group passes the owed holes below it as gaps", () =>
	withClock((clock) => {
		const producer = receivedTrack();
		const ordered = producer.subscribe({ delay: { max: Duration.millis(50) } }).ordered();
		put(producer, 0n, false);
		expect(items(ordered)).toEqual(["group 0"]);
		// Group 1 is owed; group 3 lands, then group 2 after it, which turns stale while the wall
		// clock still spares the hole at 1.
		put(producer, 3n, false);
		clock.jump(Duration.millis(30));
		put(producer, 2n, false);
		clock.jump(Duration.millis(30));
		expect(items(ordered)).toEqual(["gap 1..3 (old)", "group 3"]);
	}));

test("an ordered read's record holds only what lies ahead of it", () => {
	const producer = new TrackProducer("test");
	const sub = producer.subscribe({ delay: { max: null } });
	const ordered = sub.ordered();
	// Keyed apart, as audio by timestamp is: each group would be a run of its own.
	for (let i = 0n; i < 3000n; i++) {
		put(producer, i * 20n);
		expect(items(ordered)).toEqual([`group ${i * 20n}`]);
		expect(hooks.reportedRuns(sub)).toBeLessThanOrEqual(1);
	}
	// Unlike Rust, a group a local track produces below the read is told as nothing.
	put(producer, 10n);
	put(producer, 2998n * 20n + 10n);
	expect(items(ordered)).toEqual([]);
});

// A key-order read, as a route copy is read, folds at each hand-out while the keys it passed
// still wait to be reported.
test("a session-fed key-order read folds without losing the keys it passed", () =>
	withClock((clock) => {
		const producer = receivedTrack();
		const sub = producer.subscribe({ delay: { max: Duration.millis(50) } });
		const read = () => {
			const out: string[] = [];
			for (let drop = hooks.tryDrop(sub); drop; drop = hooks.tryDrop(sub))
				out.push(`drop ${drop.start}..${drop.end}`);
			const next = hooks.tryNextGroup(sub);
			if (next.kind === "group") {
				next.group.close();
				out.push(`group ${next.group.start}`);
			}
			for (let drop = hooks.tryDrop(sub); drop; drop = hooks.tryDrop(sub))
				out.push(`drop ${drop.start}..${drop.end}`);
			return out;
		};
		put(producer, 0n, false);
		expect(read()).toEqual(["group 0"]);
		for (let i = 1n; i <= 100n; i++) {
			// Group 3i-2 never lands, and 3i-1 turns stale behind 3i: the read passes both to hand
			// out 3i, and still reports them.
			put(producer, 3n * i, false);
			clock.jump(Duration.millis(30));
			put(producer, 3n * i - 1n, false);
			clock.jump(Duration.millis(30));
			expect(read()).toEqual([`group ${3n * i}`, `drop ${3n * i - 2n}..${3n * i}`]);
		}
		expect(hooks.reportedRuns(sub)).toBeLessThanOrEqual(2);
		sub.close();
	}));

test("a gap the bounds left out below the read never pins its record", () => {
	const producer = receivedTrack();
	const sub = producer.subscribe({ delay: { max: null } });
	const ordered = sub.ordered();
	put(producer, 0n, false);
	expect(items(ordered)).toEqual(["group 0"]);
	// Group 1 is owed, so the gap at 2 waits; the bounds then leave both out.
	drop(producer, 2n, 3n);
	expect(items(ordered)).toEqual([]);
	ordered.withRange({ start: { included: 3n } });
	for (let i = 0n; i < 1000n; i++) {
		const key = 3n + i * 20n;
		put(producer, key, false);
		producer.skipGroups(key + 1n, key + 20n);
		expect(items(ordered)).toEqual([`group ${key}`]);
	}
	expect(hooks.reportedRuns(sub)).toBeLessThanOrEqual(1);
	// The read never goes back, so widening the bounds again offers nothing below it.
	ordered.withRange({ start: { included: 0n } });
	expect(items(ordered)).toEqual([]);
});

test("a key-order read draining its drops folds its record", () => {
	const producer = new TrackProducer("test");
	const sub = producer.subscribe({ delay: { max: null } });
	// Keyed apart with the keys between skipped, so each skip is a run of its own.
	for (let i = 0n; i < 3000n; i++) {
		put(producer, i * 20n);
		producer.skipGroups(i * 20n + 1n, i * 20n + 20n);
		const read = hooks.tryNextGroup(sub);
		if (read.kind === "group") read.group.close();
		while (hooks.tryDrop(sub));
	}
	expect(hooks.reportedRuns(sub)).toBeLessThanOrEqual(1);
	sub.close();
});

test("a read returning only gaps keeps its handed-out record compact", () => {
	const producer = new TrackProducer("test");
	const sub = producer.subscribe({ delay: { max: null } });
	const ordered = sub.ordered();
	put(producer, 0n);
	expect(items(ordered)).toEqual(["group 0"]);
	for (let i = 1n; i <= 100n; i++) {
		producer.skipGroups(i * 20n - 19n, i * 20n);
		drop(producer, i * 20n, i * 20n + 1n);
		expect(items(ordered)).toEqual([`gap ${i * 20n}..${i * 20n + 1n} (delivery timeout)`]);
	}
	expect(hooks.deliveredRuns(sub)).toBe(1);
});

test("an ordered read hands out nothing past a time start still waiting to be placed", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const ordered = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1500n } }] }).ordered();
	expect(items(ordered)).toEqual([]);
	// A session's tail holds the read at the answered start's group until it lands, and says
	// nothing lies between it and 2000.
	hooks.timeStarts(producer, [[1500n, 1000n]], NO_MAX_DELAY);
	put(producer, 1000n, false);
	producer.skipGroups(1001n, 2000n);
	expect(items(ordered)).toEqual(["group 1000", "group 2000"]);
});

test("a time start folded upstream into an earlier range places from the range covering it", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const subscriber = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1500n } }] });
	// A range asked from 1000 that ends at 1200 doesn't reach the time.
	hooks.timeStarts(producer, [[1000n, 1000n, 1200n]], NO_MAX_DELAY);
	expect(hooks.servingResolved(subscriber)).toEqual([undefined]);
	// The oldest group's range, answered at 2000, served nothing at or before it.
	hooks.timeStarts(
		producer,
		[
			[1000n, 1000n, 1200n],
			[0n, 2000n, undefined],
		],
		NO_MAX_DELAY,
	);
	expect(hooks.servingResolved(subscriber)).toEqual([{ start: 0n, end: undefined, floored: false }]);
});

test("a folded time start wakes its read when the grace for a gap below it passes", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const producer = receivedTrack();
		// An oldest-group reader holds the feed open from its answered start, 1000.
		const oldest = producer.subscribe({ delay: { max: Duration.secs(1) } });
		hooks.startTrack(producer, { start: 1000n });
		hooks.timeStarts(producer, [[0n, 1000n]], NO_MAX_DELAY);
		// Group 1000's stream reset before its header: only 1800, 2000, and the skips around them
		// land, 1800 last, so the read's own budget for the time runs out after the gap's grace.
		producer.skipGroups(1001n, 1800n);
		producer.skipGroups(1801n, 2000n);
		put(producer, 2000n, false);
		clock.jump(Duration.millis(10));
		put(producer, 1800n, false);
		const ordered = producer
			.subscribe({ delay: { max: Duration.secs(1) }, ranges: [{ start: { time: 1500n } }] })
			.ordered();
		expect(items(ordered)).toEqual([]);
		let read: bigint | undefined;
		const next = ordered.nextGroup().then((group) => {
			read = group?.start;
		});
		// Let the read settle into its wait, so only a timer can wake it.
		await flush();
		clock.jump(Duration.millis(989));
		await flush();
		expect(read).toBeUndefined();
		clock.jump(Duration.millis(1));
		await flush();
		// 1800 came a full budget after 2000 superseded it.
		expect(read).toBe(2000n);
		await next;
		oldest.close();
	} finally {
		installed.close();
	}
});

test("a waiting time start holds an ordered read only within its budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const producer = receivedTrack();
		put(producer, 2000n, false);
		const ordered = producer
			.subscribe({ delay: { max: Duration.millis(100) }, ranges: [{ start: { time: 1500n } }] })
			.ordered();
		expect(items(ordered)).toEqual([]);
		let read: bigint | undefined;
		const next = ordered.nextGroup().then((group) => {
			read = group?.start;
		});
		await flush();
		clock.jump(Duration.millis(99));
		await flush();
		expect(read).toBeUndefined();
		clock.jump(Duration.millis(1));
		await flush();
		expect(read).toBe(2000n);
		await next;
	} finally {
		installed.close();
	}
});

test("a waiting time start below an ordered read's own floor holds nothing", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const ordered = producer
		.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1500n } }] })
		.withRange({ start: { included: 2000n } })
		.ordered();
	expect(items(ordered)).toEqual(["group 2000"]);
});

test("a skip is no group to place a waiting time start from", () => {
	const producer = receivedTrack();
	// GROUP 2000 says the group before it is 1000, which is still on its way.
	producer.skipGroups(1001n, 2000n);
	put(producer, 2000n, false);
	const ordered = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1500n } }] }).ordered();
	expect(items(ordered)).toEqual([]);
	put(producer, 1000n, false);
	expect(items(ordered)).toEqual(["group 1000", "group 2000"]);
});

test("a folded time start never places below the range covering it", () => {
	const producer = receivedTrack();
	put(producer, 500n, false);
	hooks.timeStarts(producer, [[1000n, 1200n, 1500n]], NO_MAX_DELAY);
	// The covering range's first group was given up, and nothing lies between it and the time.
	drop(producer, 1200n, 1201n);
	producer.skipGroups(1201n, 1600n);
	const subscriber = producer.subscribe({ delay: { max: null }, ranges: [{ start: { time: 1500n } }] });
	expect(hooks.servingResolved(subscriber)).toEqual([{ start: 1200n, end: undefined, floored: true }]);
});

test("a range answered empty holds back no later group within its budget, though it places nothing", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	// Asked with a one second budget: the answer says nothing about a group a wider reader still wants.
	hooks.timeStarts(producer, [[1000n, undefined, 1800n]], 1000);
	const at = (time: bigint) =>
		hooks.servingResolved(producer.subscribe({ delay: { max: null }, ranges: [{ start: { time } }] }));
	expect(at(1000n)).toEqual([undefined]);
	expect(at(1500n)).toEqual([undefined]);
	// GROUP 2000 says nothing lies between the time and it.
	producer.skipGroups(1000n, 2000n);
	const read = (time: bigint, max: Duration | null) =>
		items(producer.subscribe({ delay: { max }, ranges: [{ start: { time } }] }).ordered());
	for (const time of [1000n, 1500n]) {
		expect(read(time, Duration.secs(1))).toEqual(["group 2000"]);
		expect(read(time, null)).toEqual([]);
	}
});

// Run every pending microtask and the ones they queue, as a timer firing would.
async function flush() {
	for (let i = 0; i < 20; i++) await Promise.resolve();
}
