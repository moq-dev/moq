// Pairs `rs/moq-audio`'s decode-path tests of the join's backlog: a group the consumer closes below
// its cursor unread still tells the estimator when it arrived, but only when the publisher's largest
// at the subscription's start shows it was produced before.

import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { Duration, type Installed, Instant, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Time, Track, Varint } from "@moq/net";
import { race } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Jitter } from "./jitter.ts";
import { Format } from "./legacy.ts";

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
async function play(events: Event[], backlog: (group: number) => boolean): Promise<[number, number]> {
	const track = new Track.Producer("audio");
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(2000) } }), {
		format: new Format("audio"),
		delay: { max: Duration.millis(2000) },
		wait: Duration.millis(100),
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
async function join(delivered: boolean): Promise<number> {
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
	const [measured, oracle] = await play(events, (group) => group < 200);
	expect(measured).toBe(oracle);
	return measured;
}

test("a join's backlog skipped below the cursor folds nothing", async () => {
	const skipped = await join(true);
	const unseen = await join(false);
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

test("the join's boundary is the publisher's answer, not a group that raced it", async () => {
	// Group 150 lands from a cache before the publisher answers that it stood at group 200. Group 160
	// then arrives below the cursor: produced before the subscription, so it is backlog.
	const track = new Track.Producer("audio");
	const subscriber = track.subscribe({ delay: { max: Duration.millis(2000) } });
	let answered = false;
	let answer!: () => void;
	const started = new Promise<void>((resolve) => {
		answer = resolve;
	});
	const raced = new Proxy(subscriber, {
		get(target, key) {
			if (key === "start") return () => started.then(() => undefined);
			if (key === "info") return () => started.then(() => ({}));
			if (key === "largest") return async () => ({ group: answered ? 200n : 150n, frame: 0 });
			const value = Reflect.get(target, key, target);
			return typeof value === "function" ? value.bind(target) : value;
		},
	});
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const consumer = new Consumer(raced, {
		format: new Format("audio"),
		delay: { max: Duration.millis(2000) },
		wait: Duration.millis(100),
	});
	const write = (sequence: number, media: number) => {
		const group = track.createGroup(BigInt(sequence));
		const payload = new Uint8Array([...Varint.encode(media * 1000), 1]);
		group.writeFrame({ payload, timestamp: Time.Timestamp.fromMicros(media * 1000) });
		group.finish();
	};
	try {
		write(150, 0);
		await clock.advance(Duration.ZERO);
		answered = true;
		answer();
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 150n });
		write(201, 1020);
		await clock.advance(Duration.millis(200));
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 201n });
		write(160, 200);
		write(202, 1040);
		await clock.advance(Duration.ZERO);
		expect(reported).toHaveLength(1);
	} finally {
		backlog.mockRestore();
		consumer.close();
		track.finish();
		track.close();
	}
});

test("the join's boundary is the publisher's answer, not the largest by the first group", async () => {
	// The start resolves before the subscription is established, while the largest still names cached
	// group 150. The publisher answers that it stood at group 200, then groups up to 210 land before
	// the first reaches this consumer. Group 205 then arrives below the cursor: produced after the
	// subscription began, so it is no backlog, unlike group 160.
	const track = new Track.Producer("audio");
	const subscriber = track.subscribe({ delay: { max: Duration.millis(2000) } });
	let answered = false;
	let advanced = false;
	let answer!: () => void;
	const started = new Promise<void>((resolve) => {
		answer = resolve;
	});
	const raced = new Proxy(subscriber, {
		get(target, key) {
			if (key === "start") return async () => undefined;
			if (key === "info") return () => started.then(() => ({}));
			if (key === "largest") return async () => ({ group: advanced ? 210n : answered ? 200n : 150n, frame: 0 });
			const value = Reflect.get(target, key, target);
			return typeof value === "function" ? value.bind(target) : value;
		},
	});
	const reported: number[] = [];
	const backlog = spyOn(Jitter.prototype, "backlog").mockImplementation((arrival: number) => {
		reported.push(arrival);
	});
	const consumer = new Consumer(raced, {
		format: new Format("audio"),
		delay: { max: Duration.millis(2000) },
		wait: Duration.millis(100),
	});
	const write = (sequence: number, media: number) => {
		const group = track.createGroup(BigInt(sequence));
		const payload = new Uint8Array([...Varint.encode(media * 1000), 1]);
		group.writeFrame({ payload, timestamp: Time.Timestamp.fromMicros(media * 1000) });
		group.finish();
	};
	try {
		await clock.advance(Duration.ZERO);
		answered = true;
		answer();
		await clock.advance(Duration.ZERO);
		advanced = true;
		write(210, 200);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 210n });
		write(205, 100);
		await clock.advance(Duration.ZERO);
		expect(reported).toHaveLength(0);
		write(160, 0);
		await clock.advance(Duration.ZERO);
		expect(reported).toHaveLength(1);
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
	const track = new Track.Producer("audio");
	for (let sequence = 0; sequence <= 5; sequence++) {
		const group = track.createGroup(BigInt(sequence));
		const media = sequence * 20;
		group.writeFrame({
			payload: new Uint8Array([...Varint.encode(media * 1000), 1]),
			timestamp: Time.Timestamp.fromMicros(media * 1000),
		});
		group.finish();
	}
	const subscriber = track.subscribe({ delay: { max: Duration.millis(2000) } }).withRange({ end: { excluded: 3n } });
	const consumer = new Consumer(subscriber, { format: new Format("audio"), delay: { max: Duration.millis(2000) } });
	try {
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0n });
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});
