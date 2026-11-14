import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { type Group, Time, Track, Varint } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Format as LegacyFormat } from "./legacy.ts";

// Standalone from consumer.test.ts so it only pulls the Legacy format, not the CMAF decoder.

// Every test runs on its own Manual clock, so the consumer's waits move only when a test advances it.
let clock: Manual;
let installed: Installed;
beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});
afterEach(() => {
	installed.close();
});

/** Flush the consumer's readers without moving time, then return what `read` settled with, or "pending". */
async function poll<T>(read: Promise<T>): Promise<T | "pending"> {
	const flushed = clock.advance(Duration.ZERO);
	try {
		return await Promise.race([read, flushed.then(() => "pending" as const)]);
	} finally {
		await flushed;
	}
}

/** The next frame, passing over gaps and ends. */
async function nextFrame(consumer: Consumer) {
	for (;;) {
		const next = await consumer.next();
		if (!next || next.kind === "frame") return next;
	}
}

function encodeLegacyFrame(timestamp: number, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

/** Write one frame into an open group: `millis` on the timeline, tagged `tag`. */
function publishFrame(group: Group.Producer, millis: number, tag: number) {
	group.writeFrame({
		payload: encodeLegacyFrame(millis * 1000, new Uint8Array([tag])),
		timestamp: Time.Timestamp.fromMillis(millis),
	});
}

/** Publish one closed group carrying a single frame at `millis`. */
function publish(track: Track.Producer, sequence: bigint, millis: number) {
	const group = track.createGroup(sequence);
	publishFrame(group, millis, Number(sequence));
	group.finish();
}

/** Every frame the consumer will hand over without moving time, as [group, payload] pairs. */
async function drain(consumer: Consumer): Promise<[bigint, number | undefined][]> {
	const seen: [bigint, number | undefined][] = [];
	for (;;) {
		const next = await poll(consumer.next());
		if (next === "pending" || !next) break;
		if (next.kind === "frame") seen.push([next.group, next.frame.payload[0]]);
	}
	return seen;
}

// Groups go out newest first, so the head of a subscription's window arrives after the live edge
// it was served alongside. A cursor taken from the first arrival would drop that head, so it starts
// at the floor the subscription names, or the start the publisher reports, and waits for it under
// the budget. A group below that floor is never delivered and adds no gap. Mirrors Rust's
// `startup_at_the_publishers_start_over_a_newer_first_arrival`.
test("the head of the window arriving after the live edge is delivered first", async () => {
	const track = new Track.Producer("test").accept({
		timescale: Time.Timescale.MILLI,
		maxAge: Duration.millis(30_000),
	});
	const consumer = new Consumer(
		track.subscribe({ delay: { max: Duration.millis(5000) }, ranges: [{ start: { absolute: 1n } }] }),
		{ format: new LegacyFormat("data"), delay: { max: Duration.millis(5000) } },
	);

	// The live edge lands first, and delivery waits for the floor rather than starting there.
	publish(track, 3n, 30);
	const waiting = consumer.next();
	await clock.advance(Duration.millis(4_999));
	expect(await poll(waiting)).toBe("pending");

	// The window's head follows, and a reordered group below the floor with it.
	publish(track, 0n, 0);
	publish(track, 1n, 10);
	publish(track, 2n, 20);
	track.finish();

	const reads: string[] = [];
	for (let read = await waiting; read; read = await consumer.next()) {
		reads.push(read.kind === "frame" ? `${read.group}` : read.kind);
	}
	expect(reads).toEqual(["1", "2", "3"]);

	consumer.close();
});

// A group may still be downloading when its buffered frames are momentarily drained (the decode
// loop consumes faster than the network delivers). Removing it at that instant silently truncates
// its tail, so removal must wait for the group to finish.
test("a head group still downloading is not truncated", async () => {
	const track = new Track.Producer("test").accept({
		timescale: Time.Timescale.MILLI,
		maxAge: Duration.millis(30_000),
	});
	const consumer = new Consumer(
		track.subscribe({ delay: { max: Duration.millis(5000) }, ranges: [{ start: { absolute: 1n } }] }),
		{ format: new LegacyFormat("data"), delay: { max: Duration.millis(5000) } },
	);

	// The live edge group arrives first (still open), and the head after it, one frame at a time.
	const live = track.createGroup(3n);
	publishFrame(live, 3000, 30);
	const head = track.createGroup(1n);
	publishFrame(head, 1000, 10);
	const first = await nextFrame(consumer);
	expect(first?.group).toBe(1n);
	expect(first?.frame.payload[0]).toBe(10);

	// The consumer finds the group's buffer empty while it is still open. It must wait
	// rather than shift the group out, so the second frame still arrives.
	const pending = nextFrame(consumer);
	publishFrame(head, 1500, 11);
	head.finish();

	const second = await pending;
	expect(second?.group).toBe(1n);
	expect(second?.frame.payload[0]).toBe(11);

	live.finish();
	track.finish();
	track.close();
	consumer.close();
});

// A gap between two ranges is never read, so the reader walks over it at once rather than waiting a
// budget, or forever on an open track, for groups it never gets. Mirrors Rust's
// `an_excluded_gap_between_ranges_does_not_stall`.
test("an excluded gap between ranges does not stall", async () => {
	const track = new Track.Producer("test").accept({
		timescale: Time.Timescale.MILLI,
		maxAge: Duration.millis(30_000),
	});
	const consumer = new Consumer(
		track.subscribe({
			delay: { max: Duration.millis(10_000) },
			ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }, { start: { absolute: 10n } }],
		}),
		{ format: new LegacyFormat("data"), delay: { max: Duration.millis(10_000) } },
	);
	for (const sequence of [0n, 5n, 10n]) publish(track, sequence, Number(sequence) * 100);

	expect(await drain(consumer)).toEqual([
		[0n, 0],
		[10n, 10],
	]);
	consumer.close();
});

// A relative floor already resolved here still rises to where the publisher says its feed starts,
// which counts a newest group that held no frame yet, rather than wait under the budget for a group
// below it that never comes. Mirrors Rust's `a_resolved_relative_floor_rises_to_the_publishers_start`.
test("a resolved relative floor rises to the publisher's start", async () => {
	const track = new Track.Producer("test").accept({
		timescale: Time.Timescale.MILLI,
		maxAge: Duration.millis(30_000),
	});
	for (const sequence of [0n, 1n, 2n, 3n, 4n]) publish(track, sequence, Number(sequence) * 100);
	const subscriber = track.subscribe({
		delay: { max: Duration.millis(10_000) },
		ranges: [{ start: { relative: 0n } }],
	});
	expect(subscriber.floor()).toBe(4n);
	// A session declares the publisher's start; a local track has none, so stand in for it.
	subscriber.start = async () => 5n;
	const consumer = new Consumer(subscriber, {
		format: new LegacyFormat("data"),
		delay: { max: Duration.millis(10_000) },
	});

	publish(track, 5n, 500);
	expect(await drain(consumer)).toEqual([[5n, 5]]);
	consumer.close();
});
