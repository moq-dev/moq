// Pairs `rs/moq-audio`'s decode-path tests of the join's backlog: a group the ordered read gives up
// unread still tells the estimator when it landed, but only when the publisher's largest at the
// subscription's start shows it was produced before.

import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { Duration, type Installed, Instant, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Time, Track, Varint } from "@moq/net";
import { race } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Jitter } from "./jitter.ts";
import { Format } from "./legacy.ts";
import { within } from "../subscription.test-util.ts";

let clock: Manual;
let installed: Installed;
beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});
afterEach(() => {
	installed.close();
});

/** `[arrival, group, media]` in ms, in arrival order. */
type Event = [number, number, number];

/**
 * Write `events`, one frame per group, reading everything readable as each lands, and return the
 * consumer's measured term once they all have, beside the estimator fed the same arrivals with each
 * group `backlog` names reported rather than observed.
 */
async function play(
	events: Event[],
	backlog: (group: number) => boolean,
	observing = false,
): Promise<[number, number]> {
	const track = new Track.Producer("audio").accept({ timescale: Time.Timescale.MICRO, maxAge: null });
	const subscription = within(Duration.millis(2000));
	const consumer = new Consumer(track.subscribe(subscription), {
		format: new Format("audio"),
		delay: { min: Duration.millis(100), max: Duration.millis(2000) },
		ordered: observing ? track.subscribe(subscription).ordered() : undefined,
	});
	const oracle = new Jitter();
	const base = now().since(Instant.ZERO).asMillis();
	// Settles once the outstanding read does, derived once per read so each pass adds no listener to it.
	let read = consumer.next().then(() => true);
	try {
		for (const [at, sequence, media] of events) {
			await clock.advance(Duration.millis(base + at).saturatingSub(now().since(Instant.ZERO)));
			const group = track.createGroup(BigInt(sequence));
			const payload = new Uint8Array([...Varint.encode(media * 1000), 1]);
			group.writeFrame({ payload, timestamp: Time.Timestamp.fromMicros(media * 1000) });
			group.finish();
			if (backlog(sequence)) oracle.backlog(base + at);
			else oracle.observe(base + at, media);
			// Read whatever is ready, so the cursor moves as a player's does.
			for (;;) {
				const flushed = clock.advance(Duration.ZERO);
				const done = await race([read, flushed.then(() => false)]);
				await flushed;
				if (!done) break;
				read = consumer.next().then(() => true);
			}
		}
		return [consumer.spread.peek().asMillis(), oracle.measured];
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
}

/** The live edge, group 200 onward, a frame every 20 ms over a 40 ms path, arrivals moved by `delay` in order. */
function live(delay: (produced: number) => number): Event[] {
	let last = 0;
	return Array.from({ length: 300 }, (_, k) => {
		const at = Math.max(delay(20 * k), last);
		last = at;
		return [at, 200 + k, 100_000 + 20 * k];
	});
}

const order = (a: Event, b: Event) => a[0] - b[0] || a[1] - b[1];

// The publisher stood at group 200 as the subscription began. Its backlog lands behind it, 41 groups
// over the first 400 ms and 15 more 1.0 to 1.3 s in, with the live frames produced meanwhile queued
// behind them. Without `delivered`, that backlog never reaches this node, though the live frames still
// queue behind it.
async function join(delivered: boolean, observing: boolean): Promise<number> {
	const events = live((produced) => {
		const at = produced + 40;
		if (at <= 400) return 400 + produced / 10;
		if (at >= 1000 && at < 1300) return 1300 + (produced - 960) / 10;
		return at;
	});
	events[0][0] = 0;
	if (delivered) {
		for (let i = 0; i < 41; i++) events.push([5 + 10 * i, 100 + i, 90_000 + 20 * i]);
		for (let s = 0; s < 15; s++) events.push([1000 + 20 * s, 141 + s, 99_700 + 20 * s]);
	}
	events.sort(order);
	const [measured, oracle] = await play(events, (group) => group < 200, observing);
	expect(measured).toBe(oracle);
	return measured;
}

// Read through a second, ordered subscription, as the browser's audio decoder does, so the estimate
// observes the backlog as it lands. Without one, the consumer's own ordered read would say nothing of
// it: the backlog lands below where the read already is, and a local track's late arrival there is no
// gap in JS (Rust reports it Old, `a_late_arrival_is_a_gap`), so its landing never reaches the estimate.
test("a join's backlog skipped below the cursor folds nothing", async () => {
	const skipped = await join(true, true);
	const unseen = await join(false, true);
	expect(skipped).toBeLessThanOrEqual(80);
	expect(unseen).toBeGreaterThanOrEqual(280);
});

test("late groups dropped from the start hold no join open", async () => {
	// Every fifth frame arrives 300 ms late from the start: after the cursor passed it, so it is closed
	// unread, but it was produced after the subscription began, so it is no backlog. The estimator
	// measures the path from what it observes, as if the late frames never arrived.
	const late = (group: number) => group > 200 && group % 5 === 0;
	const events = live((produced) => produced + 40).map(
		([at, group, media]): Event => [at + (late(group) ? 300 : 0), group, media],
	);
	events.sort(order);
	const [dropped] = await play(events, () => false);
	const [lost] = await play(
		events.filter(([, group]) => !late(group)),
		() => false,
	);
	expect(dropped).toBe(lost);
});

/**
 * A track whose ordered read's info resolves once `answer` is called, and whose largest is what
 * `largest` says when asked. Its feed owes every key from `from`, as a session's would.
 */
function joining(from: bigint, largest: () => bigint | Promise<bigint>) {
	const track = new Track.Producer("audio").accept({ timescale: Time.Timescale.MICRO, maxAge: null });
	track.requests();
	const subscriber = track.subscribe(within(Duration.millis(2000)));
	subscriber.withRange({ start: { included: from } });
	let answer!: () => void;
	const answered = new Promise<void>((resolve) => {
		answer = resolve;
	});
	const proxy = <T extends object>(target: T, overrides: Record<string, unknown>): T =>
		new Proxy(target, {
			get(target, key) {
				if (typeof key === "string" && key in overrides) return overrides[key];
				const value = Reflect.get(target, key, target);
				return typeof value === "function" ? value.bind(target) : value;
			},
		});
	const ordered = () =>
		proxy(subscriber.ordered(), {
			info: () => answered.then(() => ({})),
			largest: async () => ({ group: await largest(), frame: 0 }),
		});
	const write = (sequence: number, media: number) => {
		const group = track.createGroup(BigInt(sequence));
		const payload = new Uint8Array([...Varint.encode(media * 1000), 1]);
		group.writeFrame({ payload, timestamp: Time.Timestamp.fromMicros(media * 1000) });
		group.finish();
	};
	return { track, subscriber: proxy(subscriber, { ordered }), answer, write };
}

// The ordered read gives up group 151 that never came with group 152 that landed behind it, once 153
// landed a budget ago. The publisher stood at group 200 as the subscription began, so the gap is
// backlog: when 152 landed reaches the estimate, once. The read hands out its first group only once
// its start is known, so a cached group can no longer race the publisher's answer.
test("a gap given up at or below the join tells the estimate when it landed", async () => {
	const { track, subscriber, answer, write } = joining(150n, () => 200n);
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const consumer = new Consumer(subscriber, {
		format: new Format("audio"),
		delay: { min: Duration.millis(100), max: Duration.millis(2000) },
	});
	try {
		answer();
		write(150, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 150n });
		await clock.advance(Duration.millis(10));
		const landed = now().since(Instant.ZERO).asMillis();
		write(152, 40);
		write(153, 60);
		await clock.advance(Duration.millis(2001));
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 153n });
		expect(reported).toEqual([landed]);
	} finally {
		backlog.mockRestore();
		consumer.close();
		track.finish();
		track.close();
	}
});

// The same gap when the publisher stood at group 151: its landing is 152's, produced since the
// subscription began, so a gap reaching past the join is no backlog.
test("a gap reaching past the join folds nothing", async () => {
	const { track, subscriber, answer, write } = joining(150n, () => 151n);
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const consumer = new Consumer(subscriber, {
		format: new Format("audio"),
		delay: { min: Duration.millis(100), max: Duration.millis(2000) },
	});
	try {
		answer();
		write(150, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 150n });
		await clock.advance(Duration.millis(10));
		write(152, 40);
		write(153, 60);
		await clock.advance(Duration.millis(2001));
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 153n });
		expect(reported).toEqual([]);
	} finally {
		backlog.mockRestore();
		consumer.close();
		track.finish();
		track.close();
	}
});

// The publisher answers that it stood at group 200, then its largest moves to 210 before the first
// group reaches this consumer. Group 206, given up with hole 205, was produced after the subscription
// began, so it is no backlog.
test("the join's boundary is the publisher's answer, not the largest by the first group", async () => {
	let advanced = false;
	const { track, subscriber, answer, write } = joining(204n, () => (advanced ? 210n : 200n));
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const consumer = new Consumer(subscriber, {
		format: new Format("audio"),
		delay: { max: Duration.millis(2000) },
	});
	try {
		answer();
		await clock.advance(Duration.ZERO);
		advanced = true;
		write(204, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 204n });
		write(206, 40);
		write(207, 60);
		await clock.advance(Duration.millis(2001));
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 207n });
		expect(reported).toEqual([]);
	} finally {
		backlog.mockRestore();
		consumer.close();
		track.finish();
		track.close();
	}
});

// A cached backlog: group 150 went stale behind 151 before the subscription, so the read gives it up and
// hands out 151 at once, both before the publisher's largest is known. The gap still waits for that
// boundary rather than being dropped with the first group.
test("a gap given up before the join's boundary is known still tells the estimate", async () => {
	let settle!: (largest: bigint) => void;
	const known = new Promise<bigint>((resolve) => {
		settle = resolve;
	});
	const { track, subscriber, answer, write } = joining(150n, () => known);
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const landed = now().since(Instant.ZERO).asMillis();
	write(150, 0);
	await clock.advance(Duration.millis(10));
	write(151, 20);
	await clock.advance(Duration.millis(2001));
	const consumer = new Consumer(subscriber, {
		format: new Format("audio"),
		delay: { min: Duration.millis(100), max: Duration.millis(2000) },
	});
	try {
		answer();
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 151n });
		expect(reported).toEqual([]);
		settle(200n);
		await clock.advance(Duration.ZERO);
		expect(reported).toEqual([landed]);
	} finally {
		backlog.mockRestore();
		consumer.close();
		track.finish();
		track.close();
	}
});

test("a range capped below the largest still delivers", async () => {
	// The track holds groups 0 to 5, and the subscriber's range ends before 3, so it never reports a
	// largest: the consumer reports no backlog, but delivers what it can read.
	const track = new Track.Producer("audio").accept({ timescale: Time.Timescale.MICRO, maxAge: null });
	for (let sequence = 0; sequence <= 5; sequence++) {
		const group = track.createGroup(BigInt(sequence));
		const media = sequence * 20;
		group.writeFrame({
			payload: new Uint8Array([...Varint.encode(media * 1000), 1]),
			timestamp: Time.Timestamp.fromMicros(media * 1000),
		});
		group.finish();
	}
	const subscriber = track.subscribe(within(Duration.millis(2000))).withRange({ end: { excluded: 3n } });
	const consumer = new Consumer(subscriber, { format: new Format("audio"), delay: { max: Duration.millis(2000) } });
	try {
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0n });
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});
