// What an ordered read returns from nextItem: groups in key order, and a gap for every group it gives up on.
// Ports the Rust `track::Ordered` item tests, so both read the same.
import { expect, test } from "bun:test";
import { Duration, now, sleep } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { StreamCode, StreamError } from "./error.ts";
import { hooks, staleHole } from "./internal.ts";
import { manual, settlesWithin, textFrame } from "./mock.ts";
import { withMax } from "./subscription.test-util.ts";
import { Timescale, Timestamp } from "./time.ts";
import { type Ordered, Producer as TrackProducer } from "./track.ts";

// A session-fed copy owes holes; an in-process producer does not.
function receivedTrack() {
	const producer = new TrackProducer("test").accept({ maxAge: null });
	hooks.receiveTrack(producer);
	return producer;
}

// A session-fed copy whose wire reports no anchors, so it places a live join from the groups it
// holds, as the Rust `received_producer` does.
function unanchoredTrack() {
	const producer = receivedTrack();
	hooks.dropAnchors(producer);
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
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	// Group 0 comes from history, as a fill would bring it; the feed owes nothing below 10.
	put(producer, 0n, false);
	put(producer, 10n, false);
	expect(items(ordered)).toEqual(["group 0", "group 10"]);
});

test("a read waits on a START pending at no floor rather than hand out the first arrival", () => {
	const producer = receivedTrack();
	hooks.startTrack(producer, {});
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	// Groups arrive newest first, so the feed may start below the first to arrive.
	put(producer, 1n, false);
	expect(items(ordered)).toEqual([]);
	hooks.startTrack(producer, { start: 0n });
	expect(items(ordered)).toEqual([]);
	put(producer, 0n, false);
	expect(items(ordered)).toEqual(["group 0", "group 1"]);
});

test("a group held behind a gap is not returned once the reader closes", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
		const producer = unanchoredTrack();
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
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
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	const ordered = producer.subscribe(withMax({ delay: { max: Duration.secs(1) } })).ordered();
	put(producer, 0n);
	put(producer, 2n);
	expect(items(ordered)).toEqual(["group 0", "group 2"]);
});

test("a gap waits behind what is owed below it, and goes out before the end", () =>
	withClock(() => {
		const producer = receivedTrack();
		const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
	const read = local.subscribe(withMax({ delay: { max: null } })).ordered();
	put(local, 0n);
	drop(local, 1n, 2n);
	local.finish();
	expect(items(read)).toEqual(["group 0", "gap 1..2 (delivery timeout)"]);
	const end = await Promise.race([read.nextItem(), sleep(Duration.millis(100)).then(() => "hung")]);
	expect(end).toBeUndefined();
});

test("a gap is returned once, even when a later drop names it again", () =>
	withClock((clock) => {
		const producer = unanchoredTrack();
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
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
		const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
		const producer = unanchoredTrack();
		put(producer, 0n, false);
		put(producer, 1n, false);
		clock.jump(Duration.millis(100));
		put(producer, 2n, false);
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
		expect(items(ordered)).toEqual(["gap 0..1 (old)", "group 1", "group 2"]);
	}));

// A gap says when the latest group it gives up landed: one the read passed as stale, and none for
// keys that never came. Mirrors the Rust `a_gap_reports_when_what_it_gives_up_landed`.
test("a gap reports when what it gives up landed", () =>
	withClock((clock) => {
		const landed = (ordered: Ordered): string[] => {
			const out: string[] = [];
			for (let item = ordered.tryNextItem(); item; item = ordered.tryNextItem()) {
				if (item.kind === "group") {
					out.push(`group ${item.group.start}`);
					item.group.close();
				} else {
					out.push(`gap ${item.start}..${item.end} ${item.landed?.since(first).asMillis() ?? "never"}`);
				}
			}
			return out;
		};
		const producer = unanchoredTrack();
		const first = now();
		clock.jump(Duration.millis(5));
		put(producer, 0n, false);
		put(producer, 1n, false);
		clock.jump(Duration.millis(100));
		put(producer, 2n, false);
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
		drop(producer, 3n, 4n);
		put(producer, 4n, false);
		// Group 0 landed 5 ms in, before it went stale, and group 3 never did.
		expect(landed(ordered)).toEqual(["gap 0..1 5", "group 1", "group 2", "gap 3..4 never", "group 4"]);
	}));

// Rust reports it Old; JS keeps no record below the read, so its memory stays bounded.
test("a local track's late arrival is no gap", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
		.subscribe(withMax({ delay: { max: null } }))
		.ordered()
		.withRange({ start: { included: 2n }, end: { excluded: 4n } });
	drop(producer, 0n, 6n);
	put(producer, 5n);
	expect(items(ordered)).toEqual(["gap 2..4 (delivery timeout)"]);
});

test("the frame helpers read past a gap", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	expect((await ordered.readFrame())?.group).toBe(0n);
	expect((await ordered.readFrame())?.group).toBe(2n);
});

test("a local track waits for what it asked a fill for, and reports a gap once the budget gives up", () =>
	withClock((clock) => {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
		const requests = producer.requests();
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
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
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
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
		const producer = unanchoredTrack();
		const ordered = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } })).ordered();
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
	const sub = producer.subscribe(withMax({ delay: { max: null } }));
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
		const producer = unanchoredTrack();
		const sub = producer.subscribe(withMax({ delay: { max: Duration.millis(50) } }));
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
	const sub = producer.subscribe(withMax({ delay: { max: null } }));
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
	const sub = producer.subscribe(withMax({ delay: { max: null } }));
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
	const sub = producer.subscribe(withMax({ delay: { max: null } }));
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
	const ordered = producer.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] })).ordered();
	expect(items(ordered)).toEqual([]);
	// A session's tail holds the read at the answered start's group until it lands, and says
	// nothing lies between it and 2000.
	hooks.timeStarts(producer, [[1500n, 1000n]]);
	put(producer, 1000n, false);
	producer.skipGroups(1001n, 2000n);
	expect(items(ordered)).toEqual(["group 1000", "group 2000"]);
});

// A time start is history, which no budget moves: an answer past its time places it, whatever max
// the reader asks elsewhere. Mirrors the Rust `an_answered_time_start_places_whatever_the_budget`.
test("an answered time start places whatever the budget", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const subscriber = producer.subscribe({ ranges: [{ start: 1500n }] });
	// The publisher started the range asked from 1500 past it, so the range starts there.
	hooks.timeStarts(producer, [[1500n, 2000n]]);
	expect(hooks.servingResolved(subscriber)).toEqual([{ start: 2000n, end: undefined, floored: true }]);
	subscriber.close();
});

// Mirrors the Rust `a_stale_drop_places_no_time_start_past_a_held_group`.
test("a stale drop places no time start past a held group", () => {
	const producer = receivedTrack();
	put(producer, 1000n, false);
	put(producer, 2000n, false);
	producer.skipGroups(1001n, 1400n);
	hooks.dropTrackGroups(producer, 1400n, 1401n, staleHole(100));
	producer.skipGroups(1401n, 2000n);
	const from = { ranges: [{ start: 1500n }] };
	const waiting = producer.subscribe(from);
	// A time start never goes stale, so it may still get 1400.
	expect(hooks.servingResolved(waiting)).toEqual([undefined]);
	waiting.close();
	producer.skipGroups(1400n, 1401n);
	const placed = producer.subscribe(from);
	expect(hooks.servingResolved(placed)[0]?.start).toBe(1000n);
	placed.close();
});

test("a time start no answer for its own time places asks upstream for it", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const subscriber = producer.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] }));
	expect(hooks.requested(producer).asks()).toEqual([1500n]);
	// Answers for other times place nothing at it: the oldest group's range starting at 2000 may
	// have expired the group containing it, and a range from 1000 says nothing past its own time.
	hooks.timeStarts(producer, [
		[0n, 2000n],
		[1000n, 1000n],
	]);
	expect(hooks.servingResolved(subscriber)).toEqual([undefined]);
	// Its own range started past it, there.
	hooks.timeStarts(producer, [[1500n, 2000n]]);
	expect(hooks.servingResolved(subscriber)).toEqual([{ start: 2000n, end: undefined, floored: true }]);
	expect(hooks.requested(producer).asks()).toEqual([]);
	subscriber.close();
});

// Mirrors the Rust `a_held_group_waits_for_a_time_start_that_may_place_at_it`.
for (const placed of [2n, 4n]) {
	test(`a group held behind a gap waits for an update's time start that may place at it (placed at ${placed})`, async () => {
		const producer = receivedTrack();
		const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
		put(producer, 0n, false);
		drop(producer, 1n, 2n);
		put(producer, 2n, false);
		expect(ordered.tryNextItem()?.group?.start).toBe(0n);
		expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
		if (placed === 4n) drop(producer, 3n, 4n);
		// Group 2 may contain time 5 until the start places.
		ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 5n }] }));
		expect(items(ordered)).toEqual([]);
		const next = ordered.nextItem();
		hooks.timeStarts(producer, [[5n, placed]]);
		if (placed === 2n) {
			expect((await next)?.group?.start).toBe(2n);
			return;
		}
		producer.skipGroups(3n, 4n);
		put(producer, 4n, false);
		expect((await next)?.group?.start).toBe(4n);
	});
}

test("a held group waiting on a time start keeps a read at its cap waiting for the start", async () => {
	const producer = receivedTrack();
	const ordered = producer
		.subscribe(withMax({ delay: { max: null } }))
		.withRange({ end: { excluded: 3n } })
		.ordered();
	put(producer, 0n, false);
	drop(producer, 1n, 2n);
	put(producer, 2n, false);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 5n }] }));
	const next = ordered.nextItem();
	// The track ends past the cap, but upstream still places the start.
	producer.finishAt({ group: 10n, frame: 0 });
	hooks.timeStarts(producer, [[5n, 2n]]);
	expect((await next)?.group?.start).toBe(2n);
});

// Mirrors the Rust `a_closed_track_releases_groups_past_a_held_group_its_join_never_placed`.
test("a closed track releases the groups past a held group its live join never placed", () => {
	const producer = receivedTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n, false);
	drop(producer, 1n, 2n);
	put(producer, 2n, false);
	producer.skipGroups(3n, 4n);
	put(producer, 4n, false);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	// Lagging further than any group here has aged, so only upstream may place it.
	const join = { start: { live: { min: Duration.secs(60) } } };
	ordered.update(withMax({ delay: { max: null }, ranges: [join, { start: 4n }] }));
	expect(items(ordered)).toEqual([]);
	producer.finishAt({ group: 5n, frame: 0 });
	producer.close();
	expect(items(ordered)).toEqual(["group 4"]);
});

// Mirrors the Rust `a_held_group_drains_before_the_tracks_abort`.
test("a held group drains before the track's abort", async () => {
	const producer = anchoredTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	const error = new StreamError(StreamCode.NotFound);
	producer.abort(error);
	expect((await ordered.nextItem())?.group?.start).toBe(2n);
	await expect(ordered.nextItem()).rejects.toBe(error);
});

// Mirrors the Rust `a_held_groups_successor_rules_out_a_time_start_past_it`.
test("a held group's successor rules out a time start past it", () => {
	const producer = anchoredTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	producer.skipGroups(3n, 4n);
	put(producer, 4n);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	// Group 4 starts at time 4, so time 5 lies in it or later, never in group 2.
	ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 4n }, { start: 5n }] }));
	expect(items(ordered)).toEqual(["group 4"]);
});

// Mirrors the Rust `a_held_groups_successor_bounds_it_by_key`.
test("a held group's successor bounds it by key, not by its first frame's time", () => {
	const producer = anchoredTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n);
	drop(producer, 1n, 2n);
	put(producer, 2n);
	const ten = producer.createGroup(10n);
	ten.writeFrame(textFrame("x", Timestamp.fromMillis(4)));
	ten.finish();
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	// Group 10 presents from time 4, but time starts resolve by key, so time 5 may lie in group 2.
	ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 5n }] }));
	expect(items(ordered)).toEqual([]);
	hooks.timeStarts(producer, [[5n, 2n]]);
	expect(items(ordered)).toEqual(["group 2"]);
});

// Mirrors the Rust `a_held_group_a_waiting_time_start_lies_past_is_passed`.
test("a group held behind a gap is passed once an update's waiting time start lies past its end", () => {
	const producer = receivedTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n, false);
	drop(producer, 1n, 2n);
	const group = producer.createGroup(2n);
	group.writeFrame(textFrame("x", Timestamp.unscaled(2n)));
	group.finishAt(3n);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	// Group 2 ends at time 3, so time 5 lies in a later group.
	ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 5n }] }));
	expect(items(ordered)).toEqual([]);
	producer.skipGroups(3n, 10n);
	put(producer, 10n, false);
	expect(items(ordered)).toEqual(["group 10"]);
});

// Mirrors the Rust `a_held_group_an_answered_time_start_placed_past_is_passed`.
test("a group held behind a gap is passed once upstream answers an update's time start past it", () => {
	const producer = receivedTrack();
	const ordered = producer.subscribe(withMax({ delay: { max: null } })).ordered();
	put(producer, 0n, false);
	drop(producer, 1n, 2n);
	put(producer, 2n, false);
	expect(ordered.tryNextItem()?.group?.start).toBe(0n);
	expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 1n, end: 2n });
	ordered.update(withMax({ delay: { max: null }, ranges: [{ start: 5n }] }));
	// The publisher started the range at 10, so group 2 holds no time it asks for.
	hooks.timeStarts(producer, [[5n, 10n]]);
	expect(items(ordered)).toEqual([]);
	producer.skipGroups(3n, 10n);
	put(producer, 10n, false);
	expect(items(ordered)).toEqual(["group 10"]);
});

test("a waiting time start holds back a gap above its group", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	drop(producer, 1500n, 1600n);
	const ordered = producer.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] })).ordered();
	expect(items(ordered)).toEqual([]);
	hooks.timeStarts(producer, [[1500n, 1000n]]);
	put(producer, 1000n, false);
	producer.skipGroups(1001n, 1500n);
	producer.skipGroups(1600n, 2000n);
	expect(items(ordered)).toEqual(["group 1000", "gap 1500..1600 (delivery timeout)", "group 2000"]);
});

test("a waiting time start below an ordered read's own floor holds nothing", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const ordered = producer
		.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] }))
		.withRange({ start: { included: 2000n } })
		.ordered();
	expect(items(ordered)).toEqual(["group 2000"]);
});

test("a waiting time start holds no gap once nothing from the read's floor through it may land", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	const ordered = () =>
		producer
			.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] }))
			.withRange({ start: { included: 1500n }, end: { excluded: 1600n } })
			.ordered();
	drop(producer, 1501n, 1600n);
	const waiting = ordered();
	expect(items(waiting)).toEqual([]);
	// The group containing the time, if still to come, lies below the read's floor.
	drop(producer, 1500n, 1501n);
	expect(items(waiting)[0]).toStartWith("gap 1500..");
	expect(items(ordered())[0]).toStartWith("gap 1500..");
});

test("a stale verdict narrower than an ordered read's budget leaves its time start waiting", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	hooks.dropTrackGroups(producer, 1000n, 1500n, staleHole(100));
	drop(producer, 1500n, 1600n);
	const ordered = producer
		.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] }))
		.withRange({ start: { included: 1000n }, end: { excluded: 1600n } })
		.ordered();
	// The group containing 1500 may still land for this budget.
	expect(items(ordered)).toEqual([]);
	put(producer, 1200n, false);
	// A group between 1200 and 1500 may still land, so the time waits for its upstream's answer.
	expect(items(ordered)).toEqual([]);
	hooks.timeStarts(producer, [[1500n, 1200n]]);
	producer.skipGroups(1201n, 1500n);
	expect(items(ordered)).toEqual(["group 1200", "gap 1500..1600 (delivery timeout)"]);
});

test("a stale verdict a capped ordered read rejects leaves its time start waiting", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	hooks.dropTrackGroups(producer, 1000n, 1500n, staleHole(10_000));
	drop(producer, 1500n, 1600n);
	const ordered = producer
		.subscribe(withMax({ delay: { max: Duration.secs(1) }, ranges: [{ start: 1500n }] }))
		.withRange({ start: { included: 1000n }, end: { excluded: 1600n } })
		.ordered();
	// Within the verdict's budget, but the cap keeps the read from the groups it rests on.
	expect(items(ordered)).toEqual([]);
	hooks.timeStarts(producer, [[1500n, 1500n]]);
	expect(items(ordered)).toEqual(["gap 1500..1600 (delivery timeout)"]);
});

// A session-fed timed copy whose wire reports where its ranges start, as the Rust `received` and
// `anchored` producer.
function anchoredTrack() {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: null });
	hooks.receiveTrack(producer);
	return producer;
}

// A stale verdict within a capped live join's budget settles none of its keys either: the cap keeps
// it from the newer groups the verdict rests on. The read asks the run again, and a gap above the
// time the join lands at holds until that answer comes.
test("a capped read asks a stale run again before passing a time start", async () => {
	const producer = anchoredTrack();
	const requests = producer.requests();
	put(producer, 900n);
	put(producer, 2000n);
	producer.skipGroups(901n, 1000n);
	hooks.dropTrackGroups(producer, 1000n, 1500n, staleHole(10_000));
	hooks.dropTrackGroups(producer, 1500n, 1600n, { error: new StreamError(StreamCode.NotFound) });
	// Half a second behind the live edge at 2000 is time 1500.
	const ordered = producer
		.subscribe({ ranges: [{ start: { live: { min: Duration.millis(500), max: Duration.secs(1) } } }] })
		.withRange({ start: { included: 900n }, end: { excluded: 1600n } })
		.ordered();
	expect(items(ordered)).toEqual(["group 900"]);
	const fill = await requests.requestedFill();
	expect([fill?.start, fill?.end]).toEqual([1000n, 1500n]);
	fill?.accept(1200n)?.finish();
	fill?.refuseGroups(1000n, 1200n, new StreamError(StreamCode.Old));
	fill?.refuseGroups(1201n, 1500n, new StreamError(StreamCode.Old));
	expect(items(ordered)).toEqual([
		"gap 1000..1200 (old)",
		"group 1200",
		"gap 1201..1500 (old)",
		"gap 1500..1600 (not found)",
	]);
	requests.close();
});

// Whether a fill is asked again within a few microtasks.
async function askedAgain(requests: ReturnType<TrackProducer["requests"]>): Promise<boolean> {
	let again = false;
	void requests.requestedFill().then((fill) => {
		if (fill) again = true;
	});
	for (let i = 0; i < 10; i++) await Promise.resolve();
	return again;
}

// A fill asks under no subscriber budget, so a run it refused as stale stays given up for the read
// waiting on it once that read widens its budget, even while the gap is still queued behind an owed
// group: nothing is asked again.
test("a widened read keeps its fill's stale refusal", async () => {
	const producer = anchoredTrack();
	const requests = producer.requests();
	put(producer, 900n);
	put(producer, 2000n);
	hooks.dropTrackGroups(producer, 1000n, 1500n, staleHole(10_000));
	const subscriber = producer
		.subscribe(withMax({ delay: { max: Duration.secs(1) } }))
		.withRange({ start: { included: 900n }, end: { excluded: 1600n } });
	const ordered = subscriber.ordered();
	// 901..1000 is still owed.
	expect(items(ordered)).toEqual(["group 900"]);
	const fill = await requests.requestedFill();
	expect([fill?.start, fill?.end]).toEqual([1000n, 1500n]);
	fill?.refuseGroups(1000n, 1500n, new StreamError(StreamCode.Old));
	// The gap waits behind 901..1000.
	expect(items(ordered)).toEqual([]);

	subscriber.update(withMax({ delay: { max: null } }));
	producer.skipGroups(901n, 1000n);
	expect(items(ordered)).toEqual(["gap 1000..1500 (old)"]);
	expect(await askedAgain(requests)).toBe(false);
	requests.close();
});

// A gap an ordered read returned stands once it widens: a read in key order never goes back.
test("a widened read keeps a stale gap it returned", async () => {
	const producer = anchoredTrack();
	const requests = producer.requests();
	put(producer, 900n);
	put(producer, 2000n);
	producer.skipGroups(901n, 1000n);
	hooks.dropTrackGroups(producer, 1000n, 1500n, staleHole(10_000));
	const subscriber = producer
		.subscribe(withMax({ delay: { max: Duration.secs(1) } }))
		.withRange({ start: { included: 900n }, end: { excluded: 1600n } });
	const ordered = subscriber.ordered();
	expect(items(ordered)).toEqual(["group 900"]);
	const fill = await requests.requestedFill();
	fill?.refuseGroups(1000n, 1500n, new StreamError(StreamCode.Old));
	expect(items(ordered)).toEqual(["gap 1000..1500 (old)"]);

	subscriber.update(withMax({ delay: { max: null } }));
	expect(items(ordered)).toEqual([]);
	expect(await askedAgain(requests)).toBe(false);
	requests.close();
});

test("a skip is no group to place a waiting time start from", () => {
	const producer = receivedTrack();
	// GROUP 2000 says the group before it is 1000, which is still on its way.
	producer.skipGroups(1001n, 2000n);
	put(producer, 2000n, false);
	const ordered = producer.subscribe(withMax({ delay: { max: null }, ranges: [{ start: 1500n }] })).ordered();
	expect(items(ordered)).toEqual([]);
	put(producer, 1000n, false);
	expect(items(ordered)).toEqual(["group 1000", "group 2000"]);
});

test("a range answered empty places its time before every group, though a time inside it waits", () => {
	const producer = receivedTrack();
	put(producer, 2000n, false);
	// The range asked from 1000 had no group at or before it.
	hooks.timeStarts(producer, [[1000n, undefined]]);
	const at = (time: bigint) => hooks.servingResolved(producer.subscribe({ ranges: [{ start: time }] }));
	expect(at(1000n)).toEqual([{ start: 0n, end: undefined, floored: false }]);
	// A time inside the range is no time it was asked from, so it waits for an answer of its own.
	expect(at(1500n)).toEqual([undefined]);
	// GROUP 2000 says nothing lies between the time and it.
	producer.skipGroups(1000n, 2000n);
	const read = (time: bigint) => items(producer.subscribe({ ranges: [{ start: time }] }).ordered());
	expect(read(1000n)).toEqual(["group 2000"]);
	expect(read(1500n)).toEqual([]);
});

// Mirrors the Rust `a_time_end_between_*` tests in `track.rs`.
const between = withMax({ ranges: [{ start: 1500n, end: 1999n }], delay: { max: Duration.millis(30_000) } });

test("a time end between two groups ends an ordered read once the publisher created one past it", async () => {
	using clock = manual();
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe(between).ordered();
	const arrival = producer.subscribe(between);
	put(producer, 1000n, false);
	expect((await ordered.nextGroup())?.start).toBe(1000n);
	expect((await arrival.recvGroup())?.start).toBe(1000n);
	const end = ordered.nextGroup();
	expect(await settlesWithin(end, clock)).toBe(false);
	// Handing out 2000 would pass every start below it; an arrival read waits, since 1500 may still come.
	put(producer, 2000n, false);
	expect(await end).toBeUndefined();
	const waiting = arrival.recvGroup();
	expect(await settlesWithin(waiting, clock)).toBe(false);
	// The ended read passed every key below its end: a group created there now is late.
	put(producer, 1500n, false);
	expect(await ordered.nextGroup()).toBeUndefined();
	expect((await waiting)?.start).toBe(1500n);

	// A group created between before a read reaches the end is still read. The time start now
	// resolves to 1500, which contains it.
	const later = producer.subscribe(between).ordered();
	expect((await later.nextGroup())?.start).toBe(1500n);
	put(producer, 1600n, false);
	expect((await later.nextGroup())?.start).toBe(1600n);
	expect(await later.nextGroup()).toBeUndefined();
});

test("a time end between appended groups ends either read at once", async () => {
	const producer = new TrackProducer("test");
	for (const key of [1000n, 2000n]) {
		const group = producer.appendGroup(Timestamp.unscaled(key));
		group.writeFrame(textFrame("x", Timestamp.unscaled(key)));
		group.finish();
	}
	const arrival = producer.subscribe(between);
	expect((await arrival.recvGroup())?.start).toBe(1000n);
	expect(await arrival.recvGroup()).toBeUndefined();
	const ordered = producer.subscribe(between).ordered();
	expect((await ordered.nextGroup())?.start).toBe(1000n);
	expect(await ordered.nextGroup()).toBeUndefined();
});

test("a time end between received groups ends an ordered read only once the skip is proven", async () => {
	using clock = manual();
	const producer = receivedTrack();
	put(producer, 1000n, false);
	put(producer, 2000n, false);
	const ordered = producer.subscribe(between).ordered();
	// The feed may still land the group containing 1500.
	const first = ordered.nextGroup();
	expect(await settlesWithin(first, clock)).toBe(false);
	producer.skipGroups(1001n, 2000n);
	expect((await first)?.start).toBe(1000n);
	expect(await ordered.nextGroup()).toBeUndefined();
});

// Mirrors the Rust `an_ordered_read_ended_past_a_stale_group_reports_it_first`.
test("an ordered read whose window ends past a group it passed as stale reports it first", () =>
	withClock((clock) => {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
		const first = producer.createGroup(0n);
		first.writeFrame(textFrame("x", Timestamp.fromMillis(0)));
		first.finishAt(10n);
		clock.jump(Duration.millis(100));
		put(producer, 100n);
		const ordered = producer
			.subscribe(withMax({ delay: { max: Duration.millis(50) }, ranges: [{ start: 0n, end: 49n }] }))
			.ordered();
		expect(items(ordered)).toEqual(["gap 0..1 (old)"]);
		expect(ordered.tryNextItem()).toBeUndefined();
		expect(ordered.tryNextGroup()).toBeUndefined();
	}));
