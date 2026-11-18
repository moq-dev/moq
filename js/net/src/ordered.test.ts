// What an ordered read returns: groups in key order, and a gap for every group it gives up on.
// Ports the Rust `track::Ordered` item tests, so both read the same.
import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { StreamCode, StreamError } from "./error.ts";
import { hooks } from "./internal.ts";
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
		const item = ordered.tryNextGroup();
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
	const read = ordered.tryNextGroup();
	expect(read?.group?.start).toBe(7n);
	drop(producer, 7n, 8n);
	cut.abort(new StreamError(StreamCode.Cancel));
	expect(items(ordered)).toEqual([]);
});

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

		const local = new TrackProducer("test");
		const read = local.subscribe({ delay: { max: null } }).ordered();
		put(local, 0n);
		drop(local, 1n, 2n);
		local.finish();
		expect(items(read)).toEqual(["group 0", "gap 1..2 (delivery timeout)"]);
		expect(read.tryNextGroup()).toBeUndefined();
	}));

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

test("a late arrival is a gap, told once", () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ delay: { max: null } }).ordered();
	put(producer, 0n);
	put(producer, 5n);
	expect(items(ordered)).toEqual(["group 0", "group 5"]);
	put(producer, 3n);
	expect(items(ordered)).toEqual(["gap 3..4 (old)"]);
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
