import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Time, Track } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer, type Playhead, type Read } from "./consumer.ts";
import { encodeFrame, Format, Producer } from "./legacy.ts";

// A group's whole end reads as an end unless the next group starts there, as the marker an older
// publisher wrote did. Every test runs on its own manual clock.
let clock: Manual;
let installed: Installed;
beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});
afterEach(() => installed.close());

const flush = () => clock.advance(Duration.ZERO);

// A track at hang's microsecond timescale, holding every group for the reader.
function micro(): Track.Producer {
	return new Track.Producer("audio").accept({ timescale: Time.Timescale.MICRO, maxAge: null });
}

// Write one media frame at `micros` into `group`.
function frame(group: { writeFrame(frame: { payload: Uint8Array; timestamp: Time.Timestamp }): void }, micros: number) {
	group.writeFrame({
		payload: encodeFrame(new Uint8Array([1]), Duration.micros(micros)),
		timestamp: Time.Timestamp.fromMicros(micros),
	});
}

/** An item as "gap <µs>+<µs>", a frame's µs, or "end <µs>". */
function describe(read: Read): string {
	if (read.kind === "gap") return `gap ${read.timestamp.asMicros()}+${read.duration.asMicros()}`;
	if (read.kind === "end") return `end ${read.end.asMicros()}`;
	return `${read.frame.timestamp.asMicros()}`;
}

// Read `track` in the background, collecting every item as `describe` writes it.
function collect(subscriber: Track.Subscriber, delay: Track.Delay, playhead?: Playhead) {
	const consumer = new Consumer(subscriber, { format: new Format("audio"), delay, playhead });
	const items: string[] = [];
	const done = (async () => {
		for (let read = await consumer.next(); read; read = await consumer.next()) items.push(describe(read));
	})();
	return { consumer, items, done };
}

const DELAY: Track.Delay = { min: Duration.millis(100), max: Duration.millis(2_000) };

test("a declared end with no marker reads like an older publisher's marker group", async () => {
	// lite-07: the group ends whole where its media stops, and the next group starts later.
	const ended = micro();
	const reader = collect(ended.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	const first = ended.appendGroup(Time.Timestamp.fromMicros(0));
	frame(first, 0);
	first.finishAt(20_000n);
	const resumed = ended.appendGroup(Time.Timestamp.fromMicros(1_000_000));
	frame(resumed, 1_000_000);
	resumed.finish();

	// An older publisher on a session with no group ends: a marker group at the end, opening with an
	// empty payload.
	const marked = micro();
	const old = collect(marked.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	const media = marked.appendGroup(Time.Timestamp.fromMicros(0));
	frame(media, 0);
	media.finish();
	const marker = marked.appendGroup(Time.Timestamp.fromMicros(20_000));
	marker.writeFrame({
		payload: encodeFrame(new Uint8Array(), Duration.micros(20_000)),
		timestamp: Time.Timestamp.fromMicros(20_000),
	});
	marker.finish();
	const later = marked.appendGroup(Time.Timestamp.fromMicros(1_000_000));
	frame(later, 1_000_000);
	later.finish();

	try {
		await flush();
		expect(reader.items).toEqual(["0", "end 20000", "1000000"]);
		expect(old.items).toEqual(reader.items);
	} finally {
		for (const [track, read] of [
			[ended, reader],
			[marked, old],
		] as const) {
			read.consumer.close();
			track.finish();
			track.close();
			await read.done;
		}
	}
});

// An eagerly cut group finishes before its successor exists, which lands only once the media past
// its newest frame has played, here with no min delay to wait out.
test("a group that starts at the declared end continues it, reading no end", async () => {
	const track = micro();
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), {
		min: Duration.ZERO,
		max: Duration.millis(100),
	});
	try {
		const first = track.appendGroup(Time.Timestamp.fromMicros(0));
		frame(first, 0);
		frame(first, 10_000);
		first.finishAt(20_000n);
		await flush();
		// Live: the successor lands as its media is captured, 10 ms after the newest frame and a bit.
		await clock.advance(Duration.millis(15));
		expect(reader.items).toEqual(["0", "10000"]);
		const next = track.appendGroup(Time.Timestamp.fromMicros(20_000));
		frame(next, 20_000);
		await flush();
		// Long past the hole wait: the successor started before it passed.
		await clock.advance(Duration.millis(1_000));
		expect(reader.items).toEqual(["0", "10000", "20000"]);
	} finally {
		reader.consumer.close();
		track.finish();
		track.close();
		await reader.done;
	}
});

// Push to talk: the encoder stops and nothing follows, so the end is all a reader gets. A min delay
// of zero, as a subscription answered over the wire reports, must not read an end at every group.
test("a push-to-talk stop reaches the reader as an end once nothing continues it", async () => {
	const track = micro();
	const wire = track.subscribe({ delay: { max: Duration.millis(30_000) } });
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), {
		min: Duration.ZERO,
		max: Duration.millis(100),
	});
	const producer = new Producer(track, new Format("audio"));
	try {
		producer.encode(new Uint8Array([1]), Duration.micros(0), true);
		producer.encode(new Uint8Array([1]), Duration.micros(20_000), false);
		producer.discontinuity(Duration.micros(40_000));
		await flush();
		expect(reader.items).toEqual(["0", "20000"]);

		// No marker on the wire: two frames, then the group's end.
		const group = await wire.recvGroup();
		const frames = [await group?.readFrame(), await group?.readFrame(), await group?.readFrame()];
		expect(frames.map((frame) => frame !== undefined)).toEqual([true, true, false]);
		expect(group?.end).toBe(40_000n);

		// The next group was due once the 20 ms past the newest frame had played since it landed, and
		// the end reads once that group would be a whole max delay late.
		await clock.advance(Duration.millis(119));
		expect(reader.items).toEqual(["0", "20000"]);
		await clock.advance(Duration.millis(1));
		expect(reader.items).toEqual(["0", "20000", "end 40000"]);

		// The next utterance starts fresh, with no second end and no gap.
		producer.encode(new Uint8Array([1]), Duration.micros(5_000_000), true);
		await flush();
		expect(reader.items).toEqual(["0", "20000", "end 40000", "5000000"]);
	} finally {
		reader.consumer.close();
		producer.finish();
		await reader.done;
	}
});

test("a declared end reads as one once the playout clock reaches it", async () => {
	const origin = now();
	// Presenting 15 ms of media now, advancing with the manual clock.
	const playhead = { now: () => Duration.millis(15).add(now().since(origin)), anchor: new Signal(0) };
	const track = micro();
	const reader = collect(
		track.subscribe({ delay: { max: Duration.millis(30_000) } }),
		{ min: Duration.millis(1_000), max: Duration.millis(2_000) },
		playhead,
	);
	try {
		const group = track.appendGroup(Time.Timestamp.fromMicros(0));
		frame(group, 0);
		group.finishAt(20_000n);
		await flush();
		await clock.advance(Duration.millis(4));
		expect(reader.items).toEqual(["0"]);
		// Well before the second-long hole wait.
		await clock.advance(Duration.millis(1));
		expect(reader.items).toEqual(["0", "end 20000"]);
	} finally {
		reader.consumer.close();
		track.finish();
		track.close();
		await reader.done;
	}
});

test("a track that finishes after a declared end ends with no end read", async () => {
	const track = micro();
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	const group = track.appendGroup(Time.Timestamp.fromMicros(0));
	frame(group, 0);
	group.finishAt(20_000n);
	track.finish();
	track.close();
	await reader.done;
	expect(reader.items).toEqual(["0"]);
	reader.consumer.close();
});

// Lost media between a declared end and the next group reads as the gap, which says it all. The end
// times the group's last frame, so the gap starts there, unless a reordered tail or a marker leaves
// the end bounding no frame.
test.each([
	["the group's end", [0, 10_000], undefined, "gap 20000+980000"],
	["the newest frame, ahead of a reordered tail", [0, 15_000, 10_000], undefined, "gap 15000+985000"],
	["an older publisher's trailing marker", [0, 10_000], 12_000, "gap 12000+988000"],
])("a gap after a declared end replaces it, starting at %s", async (_, timestamps, marker, gap) => {
	const track = micro();
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	try {
		const first = track.createGroup(0n);
		for (const timestamp of timestamps) frame(first, timestamp);
		if (marker !== undefined) {
			first.writeFrame({
				payload: encodeFrame(new Uint8Array(), Duration.micros(marker)),
				timestamp: Time.Timestamp.fromMicros(marker),
			});
		}
		first.finishAt(20_000n);
		// The publisher skipped nothing below it, so a group in between went missing.
		const later = track.createGroup(1_000_000n);
		frame(later, 1_000_000);
		await flush();
		await clock.advance(Duration.millis(100));
		expect(reader.items).toEqual([...timestamps.map(String), gap, "1000000"]);
	} finally {
		reader.consumer.close();
		track.finish();
		track.close();
		await reader.done;
	}
});

// A loss after a declared end reads the end first when every start a lost group could have taken lies
// past it: the publisher declared the run up to there skipped, so no lost group continued the media.
test("a loss past a declared skip reads the end ahead of the gap", async () => {
	const track = micro();
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	try {
		const first = track.createGroup(0n);
		frame(first, 0);
		first.finishAt(20_000n);
		// Only the starts from 990 ms on stayed undeclared, so a group lost there began past the end.
		track.skipGroups(1n, 990_000n);
		const later = track.createGroup(1_000_000n);
		frame(later, 1_000_000);
		await flush();
		await clock.advance(Duration.millis(100));
		expect(reader.items).toEqual(["0", "end 20000", "gap 20000+980000", "1000000"]);
	} finally {
		reader.consumer.close();
		track.finish();
		track.close();
		await reader.done;
	}
});

// An empty group says nothing, so one between a declared end and the media resuming leaves the end
// declared.
test("an empty group leaves a declared end pending", async () => {
	const track = micro();
	const reader = collect(track.subscribe({ delay: { max: Duration.millis(30_000) } }), DELAY);
	const first = track.appendGroup(Time.Timestamp.fromMicros(0));
	frame(first, 0);
	first.finishAt(20_000n);
	track.appendGroup(Time.Timestamp.fromMicros(20_000)).finish();
	const resumed = track.appendGroup(Time.Timestamp.fromMicros(1_000_000));
	frame(resumed, 1_000_000);
	resumed.finish();
	track.finish();
	track.close();
	await reader.done;
	expect(reader.items).toEqual(["0", "end 20000", "1000000"]);
	reader.consumer.close();
});
