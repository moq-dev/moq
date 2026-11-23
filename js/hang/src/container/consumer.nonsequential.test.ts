import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { type Group, Time, Track, Varint } from "@moq/net";
import { AudioConfigSchema } from "../catalog/audio.ts";
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

test("Consumer waits on a non-sequential hang group because it has no per-frame duration", async () => {
	// The Legacy (hang) container encodes only a timestamp per frame, no duration, so a group's
	// presentation end is unknown until the next group arrives. With non-sequential group ids we
	// therefore cannot prove the timeline is unbroken across a jump: a large id gap is
	// indistinguishable from a real missing segment that may still be in transit. So the consumer
	// waits (and lets #checkMaxDelay / #tryDurationSkip decide once the max delay budget or duration
	// coverage proves the gap too old) rather than eagerly skipping ahead, which would drop an
	// in-transit group and wreck audio.
	//
	// A hang group can only promote early once its end is known, e.g. via an end-of-group signal such
	// as a trailing frame carrying the group's end timestamp. Until the container carries that, expect
	// the wait. The CMAF path (per-sample duration in the moof) is covered in consumer.test.ts.
	// An unserved fill owes the missing groups, as a session's feed would, so the ordered read waits
	// for them up to the max delay.
	const track = new Track.Producer("test");
	track.requests();
	const subscriber = track.subscribe({ delay: { max: Duration.millis(5000) } });
	// Read from group A on, so nothing below it is owed.
	subscriber.withRange({ start: { included: 1_000_000n } });
	const consumer = new Consumer(subscriber, {
		format: new LegacyFormat("data"),
		delay: { min: null, max: Duration.millis(5000) },
	});

	// Group A at a large sequence, completed so the cursor advances past it.
	const a = track.createGroup(1_000_000n);
	a.writeFrame({
		payload: encodeLegacyFrame(0, new Uint8Array([0x01])),
		timestamp: Time.Timestamp.fromMicros(0),
	});
	a.finish();

	// Drain A's frame.
	const firstFrame = await nextFrame(consumer);
	expect(firstFrame?.frame.payload).toEqual(new Uint8Array([0x01]));

	// Park next() before the next group's frames arrive (the live streaming case).
	const pending = consumer.next();

	// Open group B at a large jump (+90_000, NOT +1) with a frame far past A's last PTS, and leave it
	// open. Legacy has no duration, so A's end is unknown -- B could be continuous, or group 1_000_001
	// could still be coming. We cannot tell, so B must NOT surface yet.
	const b = track.createGroup(1_090_000n);
	b.writeFrame({
		payload: encodeLegacyFrame(1_000_000, new Uint8Array([0x02])),
		timestamp: Time.Timestamp.fromMicros(1_000_000),
	});

	// Within the max delay budget (5s), the parked next() stays parked: no duration means no proof of
	// contiguity, so the consumer waits instead of eagerly delivering across the gap.
	await clock.advance(Duration.millis(4_999));
	expect(await poll(pending)).toBe("pending");

	consumer.close();
});

// A skip past a group already begun records where it ended, as Rust's `skip_to` does, so a later group
// continuing its timeline reads on without a gap. Sparse sequence numbers leave only the timeline to
// prove nothing is missing, and the empty group in between says nothing of it.
test("Consumer reads on without a gap past a group it skipped", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("audio"),
		delay: { max: Duration.millis(500) },
	});
	const write = (group: Group.Producer, timestamps: number[]) => {
		for (const timestamp of timestamps) {
			group.writeFrame({
				payload: encodeLegacyFrame(timestamp, new Uint8Array([0x01])),
				timestamp: Time.Timestamp.fromMicros(timestamp),
			});
		}
	};

	const first = track.createGroup(0n);
	write(first, [0, 100_000, 200_000, 300_000, 400_000]);
	for (let i = 0; i < 5; i++) expect((await nextFrame(consumer))?.group).toBe(0n);
	// The first group finishes, read to its end but not yet passed, followed by an adjacent empty group
	// and a group continuing its timeline. That group's start falls past the age bound behind the newest
	// frame, so the age skip passes the first group cleanly, then the empty one, which ends without a
	// frame and leaves only the skipped group's end to show the timeline goes on.
	first.finish();
	const empty = track.createGroup(1n);
	await clock.advance(Duration.ZERO);
	const last = track.createGroup(20n);
	write(last, [400_000, 1_000_000]);
	last.finish();
	empty.finish();
	await clock.advance(Duration.ZERO);
	const next = await poll(consumer.next());
	expect(next).toMatchObject({ kind: "frame", group: 20n });

	consumer.close();
	track.finish();
	track.close();
});

// Legacy frames carry no duration, so an audio reader reads each packet's from its codec. One-packet
// groups sequenced by their starts, as a Rust publisher numbers them, then run back to back without
// waiting on the sequences between them, while a missing one is still a gap.
test("Consumer reads one-packet Opus groups at sparse sequences back to back", async () => {
	// An unserved fill owes the missing groups, as a session's feed would.
	const track = new Track.Producer("test");
	track.requests();
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(5000) } }), {
		format: new LegacyFormat(config),
		delay: { min: null, max: Duration.millis(5000) },
	});
	// One 20 ms CELT packet per group, each read before the next lands. The group starting at 40 ms
	// never arrives.
	const packet = new Uint8Array([31 << 3, 0]);
	// Appending declares the starts between groups skipped; `after` is the start of the group before.
	const publish = (start: number, after?: number) => {
		if (after !== undefined) track.skipGroups(BigInt(after + 1), BigInt(start));
		const group = track.createGroup(BigInt(start));
		group.writeFrame({ payload: encodeLegacyFrame(start, packet), timestamp: Time.Timestamp.fromMicros(start) });
		group.finish();
	};

	publish(0);
	expect(await poll(consumer.next())).toMatchObject({ kind: "frame", group: 0n });
	const next = consumer.next();
	publish(20_000, 0);
	expect(await poll(next)).toMatchObject({ kind: "frame", group: 20_000n });
	// Nothing proves the timeline unbroken past 40 ms, so the reader waits on the missing group.
	const after = consumer.next();
	// The publisher appended a group at 40 ms, which never arrives.
	publish(60_000, 40_000);
	expect(await poll(after)).toBe("pending");

	consumer.close();
	track.finish();
	track.close();
});

// The ordered read can finish while its last group is still open, before that group's first frame
// lands. The finish says nothing of where that frame starts, so a step onto the group waits for it,
// and a frame continuing the timeline reads on without a gap.
test("Consumer judges a step onto a frameless group by its first frame, not the track's finish", async () => {
	const track = new Track.Producer("test");
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(5000) } }), {
		format: new LegacyFormat(config),
		delay: { min: null, max: Duration.millis(5000) },
	});
	// One 20 ms CELT packet per group.
	const packet = new Uint8Array([31 << 3, 0]);
	const first = track.createGroup(0n);
	first.writeFrame({ payload: encodeLegacyFrame(0, packet), timestamp: Time.Timestamp.fromMicros(0) });
	first.finish();
	expect(await poll(consumer.next())).toMatchObject({ kind: "frame", group: 0n });

	// Key 1 is never written, and the track finishes while group 2 is open and still empty.
	const last = track.createGroup(2n);
	track.finish();
	const next = consumer.next();
	expect(await poll(next)).toBe("pending");
	last.writeFrame({ payload: encodeLegacyFrame(20_000, packet), timestamp: Time.Timestamp.fromMicros(20_000) });
	last.finish();
	expect(await poll(next)).toMatchObject({ kind: "frame", group: 2n });

	consumer.close();
	track.close();
});

// A track at hang's microsecond timescale, whose groups are keyed by their start timestamps.
function micro(): Track.Producer {
	return new Track.Producer("test").accept({ timescale: Time.Timescale.MICRO, maxAge: null });
}

// Groups keyed by their start timestamps are never consecutive integers. Appending declares the starts
// between them skipped, so a slow head the age skip passes is no loss when the next group follows it,
// even where frames carry no duration to prove the timeline unbroken.
test("Consumer reads on without a gap past an aged group the publisher's skips follow", async () => {
	const track = micro();
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("data"),
		delay: { max: Duration.millis(500) },
	});
	const append = (timestamps: number[]) => {
		const group = track.appendGroup(Time.Timestamp.fromMicros(timestamps[0]));
		for (const timestamp of timestamps) {
			group.writeFrame({
				payload: encodeLegacyFrame(timestamp, new Uint8Array([0x01])),
				timestamp: Time.Timestamp.fromMicros(timestamp),
			});
		}
		return group;
	};

	const first = append([0, 100_000, 200_000, 300_000, 400_000]);
	for (let i = 0; i < 5; i++) expect((await nextFrame(consumer))?.group).toBe(0n);
	first.finish();
	await clock.advance(Duration.ZERO);
	// Far past the first group's last frame, and past the age bound, so the age skip passes it.
	append([1_000_000, 2_000_000]).finish();
	await clock.advance(Duration.ZERO);
	expect(await poll(consumer.next())).toMatchObject({ kind: "frame", group: 1_000_000n });

	consumer.close();
	track.finish();
	track.close();
});

// A buffered range runs across group boundaries wherever nothing lies between: the starts between were
// declared skipped, or the next group starts where the media before it ends.
test("Consumer buffers groups at sparse starts as one range", async () => {
	const opus = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const packet = new Uint8Array([31 << 3, 0]);
	for (const [name, format, create] of [
		// No duration: only the declared skips prove nothing is missing.
		[
			"declared",
			new LegacyFormat("data"),
			(track: Track.Producer, start: number) => track.appendGroup(Time.Timestamp.fromMicros(start)),
		],
		// Nothing declared: each 20 ms packet ends where the next group starts.
		[
			"contiguous",
			new LegacyFormat(opus),
			(track: Track.Producer, start: number) => track.createGroup(BigInt(start)),
		],
	] as const) {
		const track = micro();
		const delay = { min: Duration.millis(100), max: Duration.millis(5000) };
		// Reaching back past the first group, so the start keeps every one.
		const consumer = new Consumer(track.subscribe({ delay }), { format, delay });
		for (const start of [0, 20_000, 40_000]) {
			const group = create(track, start);
			group.writeFrame({
				payload: encodeLegacyFrame(start, packet),
				timestamp: Time.Timestamp.fromMicros(start),
			});
			group.finish();
		}
		await clock.advance(Duration.ZERO);
		expect(
			consumer.buffered.peek().map((range) => [range.start.asMicros(), range.end.asMicros()]),
			name,
		).toEqual([[0, 40_000]]);

		consumer.close();
		track.finish();
		track.close();
	}
});

// Legacy video groups, each keyed by its first frame's timestamp with nothing declared between them, as
// a moq-lite-06 hop delivers them: the frames of each in decode order, with no duration. Returns what
// the reader reads, as Rust's `legacy_video` test helper does.
async function legacyVideo(groups: number[][]): Promise<string[]> {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(10_000) } }), {
		format: new LegacyFormat("video"),
		delay: { max: Duration.millis(10_000) },
	});
	for (const pictures of groups) {
		const group = track.createGroup(BigInt(pictures[0]));
		for (const at of pictures) {
			group.writeFrame({
				payload: encodeLegacyFrame(at, new Uint8Array([0xde])),
				timestamp: Time.Timestamp.fromMicros(at),
			});
		}
		group.finish();
	}
	track.finish();
	const items: string[] = [];
	for (;;) {
		const next = await poll(consumer.next());
		if (next === "pending") throw new Error(`the reader stalled after ${items.join(", ")}`);
		if (!next) break;
		if (next.kind === "frame") items.push(`${next.frame.timestamp.asMicros()}`);
		else if (next.kind === "gap") items.push(`gap ${next.timestamp.asMicros()}+${next.duration.asMicros()}`);
		else items.push(`end ${next.end.asMicros()}`);
	}
	consumer.close();
	track.close();
	return items;
}

// The second group is an open GOP: its leading pictures present between the first group's last picture
// and its keyframe, so the two groups meet and nothing is lost.
test("an open GOP that continues the previous video group is no gap", async () => {
	expect(
		await legacyVideo([
			[0, 120_000, 40_000, 80_000],
			[280_000, 160_000, 200_000, 240_000],
		]),
	).toEqual(["0", "120000", "40000", "80000", "280000", "160000", "200000", "240000"]);
});

// A closed GOP with B-frames starts one picture past the previous group's highest timestamp, which is
// where its last picture starts rather than ends.
test("a closed GOP one picture past the previous video group is no gap", async () => {
	expect(
		await legacyVideo([
			[0, 120_000, 40_000, 80_000],
			[160_000, 280_000, 200_000, 240_000],
		]),
	).toEqual(["0", "120000", "40000", "80000", "160000", "280000", "200000", "240000"]);
});

// Field-coded open GOPs: the keyframe's second field decodes ahead of the leading pictures, so the lowest
// one comes third, within how far the first group reordered. A 40 ms frame among the second group's
// 20 ms fields allows its last picture to be one too.
test("field-coded open GOPs that continue each other are no gap", async () => {
	const read = await legacyVideo([
		[0, 20_000, 120_000, 140_000, 40_000, 60_000, 80_000, 100_000],
		[240_000, 260_000, 160_000, 180_000, 220_000],
		[380_000, 400_000, 300_000, 320_000, 340_000, 360_000],
	]);
	expect(read.filter((item) => item.startsWith("gap"))).toEqual([]);
	expect(read).toHaveLength(19);
});

// A lost group is still a gap, read ahead of the next group's keyframe, though its leading pictures would
// have presented nearer: the keyframe waits for them.
test("an open GOP past a lost video group is a gap before its keyframe", async () => {
	expect(
		await legacyVideo([
			[0, 120_000, 40_000, 80_000],
			[280_000, 160_000, 200_000, 240_000],
			// The group at 560 ms never arrives.
			[840_000, 720_000, 760_000, 800_000],
		]),
	).toEqual([
		"0",
		"120000",
		"40000",
		"80000",
		"280000",
		"160000",
		"200000",
		"240000",
		"gap 280000+560000",
		"840000",
		"720000",
		"760000",
		"800000",
	]);
});

// MP2 codes 1152 samples per frame, so its groups bridge as Opus's do, while a lost one is a gap.
test("MP2 groups at sparse sequences run back to back", async () => {
	const track = new Track.Producer("test");
	const config = AudioConfigSchema.parse({
		codec: "mp2",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(10_000) } }), {
		format: new LegacyFormat(config),
		delay: { max: Duration.millis(10_000) },
	});
	// Two 24 ms frames per group. The group starting at 96 ms never arrives.
	for (const start of [0, 48_000, 144_000]) {
		const group = track.createGroup(BigInt(start));
		for (const at of [start, start + 24_000]) {
			group.writeFrame({
				payload: encodeLegacyFrame(at, new Uint8Array([0xff, 0xfd])),
				timestamp: Time.Timestamp.fromMicros(at),
			});
		}
		group.finish();
	}
	track.finish();
	const items: string[] = [];
	for (let next = await poll(consumer.next()); next !== undefined; next = await poll(consumer.next())) {
		if (next === "pending") throw new Error(`the reader stalled after ${items.join(", ")}`);
		if (next.kind === "frame") items.push(`${next.frame.timestamp.asMicros()}`);
		else if (next.kind === "gap") items.push(`gap ${next.timestamp.asMicros()}+${next.duration.asMicros()}`);
	}
	expect(items).toEqual(["0", "24000", "48000", "72000", "gap 96000+48000", "144000", "168000"]);
	consumer.close();
	track.close();
});
