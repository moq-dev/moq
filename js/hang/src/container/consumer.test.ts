import { expect, spyOn, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Manual, type Micro, Milli, Nano, sleep } from "@moq/clock";
import { Format as LocFormat } from "@moq/loc";
import { type Group, Error as NetError, SessionCode, StreamCode, Time, Track, Varint } from "@moq/net";
import { decodeInitSegment, type InitSegment } from "../cmaf/decode.ts";
import { Format as CmafFormat } from "../cmaf/format.ts";
import { Consumer, type Read } from "./consumer.ts";
import type { Format as ContainerFormat } from "./format.ts";
import { Format as LegacyFormat, Producer as LegacyProducer } from "./legacy.ts";
import type { Frame } from "./types.ts";

const FIXTURES = JSON.parse(readFileSync(new URL("../cmaf/fixtures.json", import.meta.url), "utf8"));
function fragment(name: string): Uint8Array {
	return new Uint8Array(Buffer.from(FIXTURES[name], "base64"));
}
const TIMESCALE = 90_000;
const TEST_INIT: InitSegment = {
	timescale: TIMESCALE,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 0,
	defaultSampleSize: 0,
	defaultSampleFlags: 0,
};

function encodeLegacyFrame(timestamp: Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

/** A LOC frame carrying only the timestamp property (0x10). */
function encodeLocFrame(timestamp: Micro, payload: Uint8Array): Uint8Array {
	const props = new Uint8Array([...Varint.encode(0x10), ...Varint.encode(timestamp)]);
	return new Uint8Array([...Varint.encode(props.byteLength), ...props, ...payload]);
}

/** Write each group of `[timestamp, payload]` LOC frames to `track`, then close it. */
function writeLoc(track: Track.Producer, groups: [Micro, number[]][][]) {
	for (const frames of groups) {
		const group = track.appendGroup();
		for (const [timestamp, payload] of frames) {
			group.writeFrame({
				payload: encodeLocFrame(timestamp, new Uint8Array(payload)),
				timestamp: Time.Timestamp.fromMicros(timestamp),
			});
		}
		group.finish();
	}
	track.finish();
	track.close();
}

/** A moq-net timestamp of `ticks` at the test track's timescale. */
function at(ticks: number): Time.Timestamp {
	return new Time.Timestamp(ticks, Time.Timescale(TIMESCALE));
}

// Fixed cadence keeps timing-only tests independent of CMAF parsing.
const timedLegacy: ContainerFormat = {
	decode(payload) {
		return new LegacyFormat("video").decode(payload).map((frame) => ({ ...frame, duration: 33_333 as Micro }));
	},
};
/** A one-byte legacy frame at the test track timestamp. */
function timedFrame(data: number, timestamp: number, _sequence: number): Group.Frame {
	const payload = encodeLegacyFrame(Math.round((timestamp / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([data]));
	return { payload, timestamp: at(timestamp) };
}

/** Yield long enough for the consumer's spawned group readers to drain what's been written. */
function settle(ms = 20): Promise<void> {
	return sleep(Nano.fromMilli(Milli(ms)));
}

// Keep transport drift filtering out of container-consumer tests. These cases
// exercise the consumer's own max delay policy against a complete retained track.
// These tests write every group up front and only then read, so they ask for history
// rather than the live edge.
function replay(track: Track.Producer): Track.Subscriber {
	return track.subscribe({ maxDelay: Milli(30_000) });
}

// --- LegacyFormat ---

test("LegacyFormat decodes a valid frame", () => {
	const format = new LegacyFormat("data");
	const payload = new Uint8Array([0xde, 0xad]);
	const timestamp = 1000 as Micro;
	const frame = encodeLegacyFrame(timestamp, payload);

	const result = format.decode(frame);

	expect(result).toHaveLength(1);
	expect(result[0].timestamp).toBe(timestamp);
	expect(result[0].payload).toEqual(payload);
	expect(result[0].keyframe).toBe(false);
});

test("LegacyFormat preserves a duration marker", () => {
	const format = new LegacyFormat("video");
	const frame = encodeLegacyFrame(1000 as Micro, new Uint8Array());

	const [marker] = format.decode(frame);
	expect(marker.timestamp).toBe(1000 as Micro);
	expect(marker.payload).toHaveLength(0);
	expect(format.end(marker)).toBe(1000 as Micro);
});

test("LegacyFormat always returns keyframe: false", () => {
	const format = new LegacyFormat("data");
	const frame = encodeLegacyFrame(0 as Micro, new Uint8Array([0x01]));

	const [decoded] = format.decode(frame);
	expect(decoded.keyframe).toBe(false);
});

test("LegacyFormat always returns exactly one frame", () => {
	const format = new LegacyFormat("data");
	const frame = encodeLegacyFrame(5000 as Micro, new Uint8Array([0x01, 0x02, 0x03]));

	const result = format.decode(frame);
	expect(result).toHaveLength(1);
});

test("LegacyFormat decodes no frame from a data track's empty object and throws on a media track's", () => {
	expect(new LegacyFormat("data").decode(new Uint8Array(0))).toEqual([]);
	expect(() => new LegacyFormat("video").decode(new Uint8Array(0))).toThrow();
});

test("Legacy Producer refuses a group below the previous group start", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Micro, false);
	expect(() => producer.encode(new Uint8Array([1]), 16_000 as Micro, true)).toThrow("below the previous group start");
	producer.close();
});

test("Legacy Producer accepts B-frames within a group", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.encode(new Uint8Array([1]), 66_000 as Micro, false);
	producer.encode(new Uint8Array([1]), 33_000 as Micro, false);
	producer.close();
});

test("Legacy Producer accepts open-GOP leading pictures above the previous group", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Micro, false);
	producer.encode(new Uint8Array([1]), 66_000 as Micro, true);
	producer.encode(new Uint8Array([1]), 50_000 as Micro, false);
	producer.close();
});

test("Legacy Producer writes a duration marker at the next keyframe", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxDelay: Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([0xde, 0xad]), 0 as Micro, true);
	producer.encode(new Uint8Array([0xbe, 0xef]), 10_000 as Micro, false);
	producer.encode(new Uint8Array([0xca, 0xfe]), 20_000 as Micro, true);
	producer.close();

	const group = await subscriber.recvGroup();
	expect(group).toBeDefined();
	const frames: { timestamp: Micro; size: number }[] = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) break;
		const [timestamp, payload] = Varint.decode(frame.payload);
		frames.push({ timestamp: timestamp as Micro, size: payload.byteLength });
	}
	expect(frames).toEqual([
		{ timestamp: 0 as Micro, size: 2 },
		{ timestamp: 10_000 as Micro, size: 2 },
		{ timestamp: 20_000 as Micro, size: 0 },
	]);
});

test("Legacy Producer omits a reordered group's presentation endpoint marker", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxDelay: Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	for (const [index, timestamp] of [0, 120_000, 40_000, 80_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Micro, index === 0);
	}
	producer.encode(new Uint8Array([1]), 160_000 as Micro, true);
	producer.discontinuity(200_000 as Micro);
	producer.close();
	const group = await subscriber.recvGroup();
	expect(group).toBeDefined();
	let count = 0;
	while (await group?.readFrame()) count++;
	expect(count).toBe(4);
	const next = await subscriber.recvGroup();
	expect(next).toBeDefined();
	await next?.readFrame();
	const marker = await next?.readFrame();
	expect(marker).toBeDefined();
	if (!marker) throw new Error("expected a presentation endpoint marker");
	const [timestamp, payload] = Varint.decode(marker.payload);
	expect(timestamp).toBe(200_000);
	expect(payload.byteLength).toBe(0);
});

test("Legacy Producer estimates the tail from the current cadence", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxDelay: Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	for (const [index, timestamp] of [0, 16_000, 32_000, 65_000, 98_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Micro, index === 0);
	}
	producer.close();
	const group = await subscriber.recvGroup();
	expect(group).toBeDefined();
	let end = 0;
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) break;
		const [timestamp, payload] = Varint.decode(frame.payload);
		if (!payload.byteLength) end = timestamp;
	}
	expect(end).toBe(131_000);
});

test("Legacy Producer rejects a backwards discontinuity without closing the group", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxDelay: Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Micro, true);
	expect(() => producer.discontinuity(10_000 as Micro)).toThrow();
	producer.encode(new Uint8Array([2]), 30_000 as Micro, false);
	producer.discontinuity(35_000 as Micro);
	producer.close();
	const group = await subscriber.recvGroup();
	const timestamps = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) break;
		timestamps.push(Varint.decode(frame.payload)[0]);
	}
	expect(timestamps).toEqual([20_000, 30_000, 35_000]);
});

test("Legacy Producer keeps the cadence after rejecting a backwards discontinuity", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxDelay: Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Micro, true);
	producer.encode(new Uint8Array([2]), 30_000 as Micro, false);
	expect(() => producer.discontinuity(10_000 as Micro)).toThrow();
	producer.close();
	const group = await subscriber.recvGroup();
	const timestamps = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) break;
		timestamps.push(Varint.decode(frame.payload)[0]);
	}
	expect(timestamps).toEqual([20_000, 30_000, 40_000]);
});

test("Legacy Producer refuses a keyframe that rewinds the timeline", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Micro, true);
	producer.encode(new Uint8Array([2]), 30_000 as Micro, false);
	expect(() => producer.encode(new Uint8Array([3]), 0 as Micro, true)).toThrow("below the previous group start");
	producer.close();
});

/** Read every group a subscriber is served until `last`, as (sequence, [timestamp, payload size][]). */
async function readGroups(subscriber: Track.Subscriber, last: number) {
	const groups: [number, [number, number][]][] = [];
	for (;;) {
		const group = await subscriber.recvGroup();
		if (!group) throw new Error("track ended before the last group");
		const frames: [number, number][] = [];
		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;
			const [timestamp, payload] = Varint.decode(frame.payload);
			frames.push([timestamp, payload.byteLength]);
		}
		groups.push([group.sequence, frames]);
		if (group.sequence === last) return groups;
	}
}

test("Legacy Producer discontinuity marks the break with one empty frame at the live edge", async () => {
	const track = new Track.Producer("test");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("audio"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.encode(new Uint8Array([1]), 20_000 as Micro, true);
	producer.discontinuity();
	producer.discontinuity(); // nothing new to mark
	producer.encode(new Uint8Array([1]), 5_000_000 as Micro, true);
	producer.close();

	expect(await readGroups(subscriber, 3)).toEqual([
		[0, [[0, 1]]],
		[1, [[20_000, 1]]],
		[2, [[20_000, 0]]],
		[3, [[5_000_000, 1]]],
	]);
});

test("Legacy Producer discontinuity marks the break at the caller's end", async () => {
	const track = new Track.Producer("test");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.discontinuity(33_000 as Micro);
	producer.close();

	expect(await readGroups(subscriber, 1)).toEqual([
		[
			0,
			[
				[0, 1],
				[33_000, 0],
			],
		],
		[1, [[33_000, 0]]],
	]);
});

test("Legacy Producer discontinuity writes no end estimated from the cadence", async () => {
	const track = new Track.Producer("test");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	for (const [index, timestamp] of [0, 33_000, 66_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Micro, index === 0);
	}
	producer.discontinuity();
	// The capture swap resumes sooner than one frame later: no end past it, so no rewind.
	producer.encode(new Uint8Array([1]), 80_000 as Micro, true);
	producer.close();

	expect(await readGroups(subscriber, 2)).toEqual([
		[
			0,
			[
				[0, 1],
				[33_000, 1],
				[66_000, 1],
			],
		],
		[1, [[66_000, 0]]],
		[2, [[80_000, 1]]],
	]);
});

test("Legacy Producer discontinuity marks nothing before any frame", async () => {
	const empty = new Track.Producer("empty");
	new LegacyProducer(empty, new LegacyFormat("video")).discontinuity();
	expect(empty.appendGroup().sequence).toBe(0);
});

// An empty payload is data on a data track, so its marker is an object with no bytes, which reads
// as an end where the reader got to rather than as a gap.
test("Legacy Producer discontinuity on a data track reads as an end at the edge", async () => {
	const track = new Track.Producer("data");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("data"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.encode(new Uint8Array(), 20_000 as Micro, true);
	producer.discontinuity();
	producer.encode(new Uint8Array([1]), 5_000_000 as Micro, true);
	producer.close();

	const consumer = new Consumer(subscriber, { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });
	const items: string[] = [];
	for (let read = await consumer.next(); read; read = await consumer.next()) items.push(describe(read));
	expect(items).toEqual(["0", "20000", "end 20000", "5000000"]);
	consumer.close();
});

// A group's reach runs to its successor's first frame, so without the marker the group before a
// pause would stretch across the whole gap and read as live to anyone joining after the resume.
// A data track's marker is alone in its group, so one sharing it is refused rather than read as
// a break or dropped in silence.
test("Consumer refuses a data marker that shares its group", async () => {
	for (const second of [new Uint8Array(), encodeLegacyFrame(10_000 as Micro, new Uint8Array([1]))]) {
		const track = new Track.Producer("data");
		const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });
		try {
			const group = track.appendGroup();
			group.writeFrame({ payload: new Uint8Array(), timestamp: Time.Timestamp.fromMicros(0) });
			group.writeFrame({ payload: second, timestamp: Time.Timestamp.fromMicros(10_000) });
			group.finish();
			// A finished track resolves the read instead of hanging when the reader fails to end.
			track.finish();
			track.close();
			await expect(nextFrame(consumer)).rejects.toThrow("shares its group");
		} finally {
			consumer.close();
			track.finish();
			track.close();
		}
	}
});

test("Legacy Producer discontinuity keeps pre-pause media from reading as live", async () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Micro, false);
	producer.discontinuity();
	producer.encode(new Uint8Array([1]), 5_000_000 as Micro, true);
	producer.close();

	const subscriber = track.subscribe({ maxDelay: Milli(1_000) });
	expect((await readGroups(subscriber, 2)).map(([sequence]) => sequence)).toEqual([1, 2]);
});

test("LegacyFormat throws on truncated input", () => {
	const format = new LegacyFormat("data");
	// A varint that indicates more bytes follow but is truncated
	expect(() => format.decode(new Uint8Array([0x80]))).toThrow();
});

// --- CmafFormat ---

test("CmafFormat decodes a valid keyframe segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = fragment("key");

	const result = format.decode(segment, at(0));

	expect(result).toHaveLength(1);
	expect(result[0].payload).toEqual(new Uint8Array([0xca, 0xfe]));
	expect(result[0].timestamp).toBe(0 as Micro);
	expect(result[0].keyframe).toBe(true);
});

// Packagers flag every audio sample a sync sample. Reported as a keyframe, each would open
// its own group, so audio never decodes one: the consumer marks the group start instead.
test("CmafFormat never reports an audio keyframe", () => {
	const init = decodeInitSegment(fragment("audioInit"));
	expect(init.kind).toBe("audio");

	const format = new CmafFormat(init);
	const segment = fragment("audio");

	const result = format.decode(segment, at(0));
	expect(result).toHaveLength(1);
	expect(result[0].keyframe).toBe(false);
});

test("CMAF init rejects an unsupported track handler", () => {
	const init = fragment("audioInit");
	const offset = Buffer.from(init).indexOf("soun");
	expect(offset).toBeGreaterThanOrEqual(0);
	init.set(new TextEncoder().encode("text"), offset);
	expect(() => decodeInitSegment(init)).toThrow("Unsupported track handler: text");
});

test("CmafFormat decodes a delta frame segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = fragment("delta");

	const result = format.decode(segment, at(3000));

	expect(result).toHaveLength(1);
	expect(result[0].keyframe).toBe(false);
});

test("CmafFormat converts timescale units to microseconds", () => {
	const format = new CmafFormat(TEST_INIT);
	// 90000 timescale units = 1 second = 1_000_000 microseconds
	const segment = fragment("oneSecond");

	const result = format.decode(segment, at(TIMESCALE));
	expect(result[0].timestamp).toBe(1_000_000 as Micro);
});

test("CmafFormat throws on corrupt segment", () => {
	const format = new CmafFormat(TEST_INIT);
	expect(() => format.decode(new Uint8Array([0x00, 0x01, 0x02]), at(0))).toThrow();
});

// --- Consumer ---

function encodeLegacy(timestamp: Micro): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const payload = new Uint8Array([0xde, 0xad]);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

// `stamp` is the transport timestamp, the container's own by default.
function writeGroupWithLegacyFrames(
	track: Track.Producer,
	sequence: number,
	timestamps: Micro[],
	stamp: (ts: Micro) => Time.Timestamp = (ts) => Time.Timestamp.fromMicros(ts),
) {
	const group = track.createGroup(sequence);
	for (const ts of timestamps) {
		group.writeFrame({ payload: encodeLegacy(ts), timestamp: stamp(ts) });
	}
	group.finish();
}

function writeMarkerGroup(track: Track.Producer, sequence: number, timestamp: Micro) {
	const group = track.createGroup(sequence);
	group.writeFrame({
		payload: encodeLegacyFrame(timestamp, new Uint8Array()),
		timestamp: Time.Timestamp.fromMicros(timestamp),
	});
	group.finish();
}

async function drainFrames(
	consumer: Consumer,
	timeout: number,
): Promise<{ timestamp: Micro; group: number; keyframe: boolean }[]> {
	const frames: { timestamp: Micro; group: number; keyframe: boolean }[] = [];
	for (;;) {
		const result = await Promise.race([consumer.next(), sleep(Nano.fromMilli(Milli(timeout))).then(() => null)]);
		if (result === null || result === undefined) break;
		if (result.kind === "frame") {
			frames.push({ timestamp: result.frame.timestamp, group: result.group, keyframe: result.frame.keyframe });
		}
	}
	return frames;
}

/** An item as "gap <µs>+<µs>", a frame's µs, or "end <µs>". */
function describe(read: Read): string {
	if (read.kind === "gap") return `gap ${read.timestamp}+${read.duration}`;
	if (read.kind === "end") return `end ${read.end}`;
	return `${read.frame.timestamp}`;
}

/** The frame a read carries, failing on anything else. */
function frameOf(read: Read | string | undefined): Frame {
	if (typeof read !== "object" || read.kind !== "frame")
		throw new Error(`expected a frame, got ${JSON.stringify(read)}`);
	return read.frame;
}

test("Consumer delivers frames from a single group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(2);
	expect(frames[0].timestamp).toBe(0 as Micro);
	expect(frames[1].timestamp).toBe(33_000 as Micro);
	consumer.close();
});

test("Consumer forces keyframe true at index 0", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames[0].keyframe).toBe(true);
	expect(frames[1].keyframe).toBe(false);
	consumer.close();
});

test("Consumer index spans MoQ frames for keyframe detection", async () => {
	// Custom format that returns 3 samples per MoQ frame, all keyframe: false
	const multiFormat: ContainerFormat = {
		decode(_frame: Uint8Array): Frame[] {
			return [
				{ payload: new Uint8Array([1]), timestamp: 0 as Micro, keyframe: false },
				{ payload: new Uint8Array([2]), timestamp: 33_000 as Micro, keyframe: false },
				{ payload: new Uint8Array([3]), timestamp: 66_000 as Micro, keyframe: false },
			];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: multiFormat, maxDelay: 500 as Milli });

	const group = track.createGroup(0);
	group.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() }); // first MoQ frame → 3 samples
	group.writeFrame({ payload: new Uint8Array([0x02]), timestamp: Time.Timestamp.now() }); // second MoQ frame → 3 samples
	group.finish();
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(6);
	// Only index 0 is keyframe, rest are false
	expect(frames.map((f) => f.keyframe)).toEqual([true, false, false, false, false, false]);
	consumer.close();
});

// A malformed payload is not loss: the reader ends with the decode error where it sits, after the
// frames ahead of it, instead of reading the rest of the track as if the group were cut short.
for (const [where, payloads] of [
	["at the head of a group", [0xff, 0x01]],
	["mid-group", [0x01, 0xff, 0x02]],
] as const) {
	test(`Consumer ends the reader on a decode error ${where}`, async () => {
		// 0xFF in the first byte signals the format to throw, which keeps this deterministic
		// when groups decode in parallel.
		const failingFormat: ContainerFormat = {
			decode(frame: Uint8Array): Frame[] {
				if (frame[0] === 0xff) throw new Error("malformed");
				return [{ payload: frame, timestamp: frame[0] as Micro, keyframe: false }];
			},
		};

		const track = new Track.Producer("test");
		const consumer = new Consumer(replay(track), { format: failingFormat, maxDelay: 500 as Milli });

		const g0 = track.createGroup(0);
		for (const byte of payloads)
			g0.writeFrame({ payload: new Uint8Array([byte]), timestamp: Time.Timestamp.now() });
		g0.finish();

		// A later group that decodes cleanly is never reached.
		const g1 = track.createGroup(1);
		g1.writeFrame({ payload: new Uint8Array([0x04]), timestamp: Time.Timestamp.now() });
		g1.finish();
		track.finish();
		track.close();

		const frames: number[] = [];
		const read = async () => {
			for (;;) {
				const result = await consumer.next();
				if (!result) return result;
				if (result.kind === "frame") frames.push(result.frame.timestamp);
			}
		};
		await expect(read()).rejects.toThrow("malformed");
		expect(frames).toEqual(payloads.slice(0, payloads.indexOf(0xff)));
		consumer.close();
	});
}

// Errors come in delivery order, so a decode error waits behind the items ahead of it, a gap included.
test("Consumer delivers everything ahead of a decode error", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 10_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [100_000 as Micro]);
	const malformed = track.createGroup(3);
	// A truncated timestamp varint.
	malformed.writeFrame({ payload: new Uint8Array([0x80]), timestamp: Time.Timestamp.fromMicros(200_000) });
	malformed.finish();
	track.finish();
	track.close();

	const items: string[] = [];
	const read = async () => {
		for (let item = await consumer.next(); item; item = await consumer.next()) items.push(describe(item));
	};
	await expect(read()).rejects.toThrow("buffer too short");
	expect(items).toEqual(["0", "gap 0+100000", "100000"]);
	consumer.close();
});

// Groups decode in parallel, so a later group's failure must not replace the error the reader ended with.
test("Consumer keeps the first of several decode errors", async () => {
	const failingFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			throw new Error(`malformed ${frame[0]}`);
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: failingFormat, maxDelay: 500 as Milli });

	for (const sequence of [0, 1]) {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: new Uint8Array([sequence]), timestamp: Time.Timestamp.now() });
		group.finish();
	}
	track.finish();
	track.close();
	await settle();

	await expect(consumer.next()).rejects.toThrow("malformed 0");
	await expect(consumer.next()).rejects.toThrow("malformed 0");
	consumer.close();
});

// An error ends the reader, so nothing keeps reading, decoding, or holding the groups still open.
test("Consumer releases its open groups when an error ends it", async () => {
	let resolveOpen!: () => void;
	const open = new Promise<void>((resolve) => {
		resolveOpen = resolve;
	});
	const failingFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			if (frame[0] === 0xff) throw new Error("malformed");
			if (frame[0] === 0x01) resolveOpen();
			return [{ payload: frame, timestamp: frame[0] as Micro, keyframe: false }];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: failingFormat, maxDelay: 500 as Milli });

	const malformed = track.createGroup(0);
	malformed.writeFrame({ payload: new Uint8Array([0xff]), timestamp: Time.Timestamp.now() });
	malformed.finish();
	const later = track.createGroup(1);
	later.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() });
	await open;
	expect(later.demand().wanted.peek()).toBe(true);

	await expect(consumer.next()).rejects.toThrow("malformed");
	expect(later.demand().wanted.peek()).toBe(false);
	consumer.close();
});

// A transport reset is loss: frames decoded before it are kept and the cut tail reads as a gap.
test("Consumer keeps frames decoded before a group reset (truncated GoP)", async () => {
	// Reset only once both frames are decoded: a reset discards whatever is still unread.
	const format = new LegacyFormat("data");
	const decode = format.decode.bind(format);
	let decodes = 0;
	let resolveDecoded!: () => void;
	const decoded = new Promise<void>((resolve) => {
		resolveDecoded = resolve;
	});
	format.decode = (frame) => {
		const frames = decode(frame);
		if (++decodes === 2) resolveDecoded();
		return frames;
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format, maxDelay: 500 as Milli });

	const g0 = track.createGroup(0);
	for (const timestamp of [1, 2]) {
		g0.writeFrame({
			payload: encodeLegacyFrame(timestamp as Micro, new Uint8Array([1])),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
	}
	await decoded;
	g0.abort(new NetError.Stream(StreamCode.Cancel));

	writeGroupWithLegacyFrames(track, 1, [4 as Micro]);
	track.finish();
	track.close();

	const items: string[] = [];
	for (let read = await consumer.next(); read; read = await consumer.next()) items.push(describe(read));
	expect(items).toEqual(["1", "2", "gap 2+2", "4"]);
	consumer.close();
});

test("Consumer close returns undefined from next()", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	const promise = consumer.next();
	consumer.close();

	const result = await promise;
	expect(result).toBeUndefined();
});

test("Consumer throws on concurrent next() calls", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	// First call blocks waiting for data
	void consumer.next();

	// Second call should throw
	expect(() => consumer.next()).toThrow("multiple calls to next not supported");
	consumer.close();
});

test("Consumer skips groups via PTS-span when over the max delay", async () => {
	const track = new Track.Producer("test");
	// Zero max delay = skip everything that's not the latest
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 0 as Milli });

	// Write groups with increasing timestamps. With a 0 max delay, any PTS span > 0 triggers skip.
	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Micro]);
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 300);
	// With a zero max delay, the consumer should skip to the latest group
	const groups = [...new Set(frames.map((f) => f.group))];
	expect(groups.at(-1)).toBe(2);
	consumer.close();
});

test("Consumer measures arrivals as they land", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("audio"),
		maxDelay: 5000 as Milli,
	});
	const next = new Track.Producer("next");

	try {
		// Nothing observed yet: the cold-start estimate.
		expect(consumer.spread.peek()).toBe(100 as Milli);

		// Groups arriving exactly as fast as their media, across four resample intervals. Nothing is
		// late, so the evidence pulls the estimate down from the cold start.
		for (let i = 0; i < 5; i++) {
			writeGroupWithLegacyFrames(track, i, [(i * 520_000) as Micro]);
			await clock.advance(Nano.fromMilli(Milli(520)));
		}
		expect(consumer.spread.peek()).toBeLessThan(100 as Milli);

		// A replacement on the same path continues the estimate rather than restarting cold.
		const replacement = new Consumer(next.subscribe(), {
			format: new LegacyFormat("audio"),
			maxDelay: 5000 as Milli,
			predecessor: consumer,
		});
		expect(replacement.spread.peek()).toBe(consumer.spread.peek());
		replacement.close();
	} finally {
		track.finish();
		track.close();
		consumer.close();
		next.finish();
		next.close();
		installed.close();
	}
});

// --- Ordering ---

test("Consumer delivers groups in sequence order regardless of arrival order", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	writeGroupWithLegacyFrames(track, 2, [60_000 as Micro]);
	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [30_000 as Micro]);
	track.finish();
	track.close();

	await sleep(Nano.fromMilli(Milli(100)));

	const frames = await drainFrames(consumer, 500);
	expect(frames).toHaveLength(3);
	expect(frames[0].group).toBe(0);
	expect(frames[1].group).toBe(1);
	expect(frames[2].group).toBe(2);
	consumer.close();
});

test("Consumer delivers a group that arrives below the cursor", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	// Group.Producer 5 arrives first (sets active = 5)
	writeGroupWithLegacyFrames(track, 5, [100_000 as Micro]);
	await sleep(Nano.fromMilli(Milli(50)));

	// Group.Producer 3 lands behind it, and Group.Producer 6 ahead of it. Arriving below the
	// cursor is not what makes content stale: how far behind the live edge a group may be is
	// the subscription's own max delay, which `replay` deliberately opens wide here. So all
	// three are handed over, in sequence order, since delivery has not passed any of them yet.
	writeGroupWithLegacyFrames(track, 3, [0 as Micro]);
	writeGroupWithLegacyFrames(track, 6, [200_000 as Micro]);
	track.finish();
	track.close();

	await sleep(Nano.fromMilli(Milli(100)));

	const frames = await drainFrames(consumer, 500);
	expect(frames.map((frame) => frame.group)).toEqual([3, 5, 6]);
	consumer.close();
});

// --- Group.Producer boundary signals ---

test("Consumer next() yields no gap before the first frame", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Micro]);
	track.finish();
	track.close();

	await sleep(Nano.fromMilli(Milli(50)));

	const seen: string[] = [];
	for (;;) {
		const result = await Promise.race([consumer.next(), sleep(Nano.fromMilli(Milli(500))).then(() => null)]);
		if (result === null || result === undefined) break;
		seen.push(describe(result));
	}

	expect(seen).toEqual(["0", "33000", "66000"]);
	consumer.close();
});

test("Consumer reads nothing after close, even the frame a gap was returned ahead of", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 0 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);

	// Group 1 never arrives, so group 2's frame is deferred behind a gap.
	writeGroupWithLegacyFrames(track, 2, [100_000 as Micro]);
	await settle();
	expect(await consumer.next()).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 100_000 as Micro });

	consumer.close();
	expect(await consumer.next()).toBeUndefined();
	track.finish();
	track.close();
});

test("Consumer keeps a trailing duration marker internal", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("video"),
		maxDelay: 500 as Milli,
	});

	const group = track.createGroup(0);
	group.writeFrame({
		payload: encodeLegacyFrame(0 as Micro, new Uint8Array([0xde, 0xad])),
		timestamp: Time.Timestamp.now(),
	});
	group.writeFrame({
		payload: encodeLegacyFrame(33_000 as Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	group.finish();
	track.finish();
	track.close();
	await settle();

	const media = frameOf(await consumer.next());
	expect(media.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media.keyframe).toBe(true);
	expect(media.duration).toBeUndefined();
	// The marker is the frame's duration, not an item.
	expect(await consumer.next()).toBeUndefined();
	consumer.close();
});

test("Consumer reads a leading marker as an end and keeps the first media keyframe", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("video"),
		maxDelay: 500 as Milli,
	});

	const group = track.createGroup(0);
	group.writeFrame({
		payload: encodeLegacyFrame(20_000 as Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	group.writeFrame({
		payload: encodeLegacyFrame(20_000 as Micro, new Uint8Array([0xde, 0xad])),
		timestamp: Time.Timestamp.now(),
	});
	group.finish();
	track.finish();
	track.close();

	expect(await consumer.next()).toEqual({ kind: "end", group: 0, end: 20_000 as Micro });
	const media = frameOf(await consumer.next());
	expect(media.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media.keyframe).toBe(true);
	expect(media.duration).toBeUndefined();
	consumer.close();
});

test("Consumer reads an empty LOC payload as the frame's duration", async () => {
	const track = new Track.Producer("test");
	writeLoc(track, [
		[
			[0 as Micro, [0xde, 0xad]],
			[33_000 as Micro, []],
			[66_000 as Micro, []],
		],
	]);

	const consumer = new Consumer(replay(track), { format: new LocFormat("video"), maxDelay: 500 as Milli });
	await settle();
	const media = frameOf(await consumer.next());
	expect(media.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media.duration).toBeUndefined();
	expect(await consumer.next()).toBeUndefined();
	consumer.close();
});

test("Consumer passes over LOC duration markers", async () => {
	const track = new Track.Producer("test");
	writeLoc(track, [
		[
			[0 as Micro, [0xde, 0xad]],
			[10_000 as Micro, [0xbe, 0xef]],
			[20_000 as Micro, []],
		],
		[
			[20_000 as Micro, [0xca, 0xfe]],
			[30_000 as Micro, []],
		],
	]);

	const consumer = new Consumer(replay(track), { format: new LocFormat("video"), maxDelay: 500 as Milli });
	await settle();
	const first = frameOf(await consumer.next());
	expect(first.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(first.keyframe).toBe(true);
	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0xbe, 0xef]));
	const next = frameOf(await consumer.next());
	expect(next.payload).toEqual(new Uint8Array([0xca, 0xfe]));
	expect(next.keyframe).toBe(true);
	expect(await consumer.next()).toBeUndefined();
	consumer.close();
});

test("Consumer preserves empty Legacy and LOC data frames", async () => {
	for (const kind of ["legacy", "loc"]) {
		const track = new Track.Producer("data");
		if (kind === "legacy") {
			const producer = new LegacyProducer(track, new LegacyFormat("data"));
			producer.encode(new Uint8Array(), 0 as Micro, true);
			producer.close();
		} else {
			writeLoc(track, [[[0 as Micro, []]]]);
		}
		const format = kind === "legacy" ? new LegacyFormat("data") : new LocFormat();
		const consumer = new Consumer(replay(track), { format, maxDelay: 500 as Milli });
		const frame = frameOf(await consumer.next());
		expect(frame.payload).toEqual(new Uint8Array());
		expect(frame.keyframe).toBe(true);
		consumer.close();
	}
});

// --- Malformed rewind ---

/** Read until the next media frame, passing over gaps and ends. */
async function nextFrame(consumer: Consumer) {
	for (;;) {
		const result = await consumer.next();
		if (!result || result.kind === "frame") return result;
	}
}

// The track refuses a transport timestamp that rewinds, so these rewind only the container's
// own timestamps, under wall-clock transport stamps that rise in write order.
const wallClock = () => Time.Timestamp.now();

test("Consumer aborts a group below the previous group start", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [10_000_000 as Micro], wallClock);
	writeGroupWithLegacyFrames(track, 1, [0 as Micro, 100_000 as Micro], wallClock);
	await settle();

	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(10_000_000 as Micro);
	await expect(nextFrame(consumer)).rejects.toThrow("below the previous group start");

	consumer.close();
});

// A later group's rewind does not mask an earlier group's decode error.
test("Consumer throws the first error in delivery order", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [10_000_000 as Micro], wallClock);
	const malformed = track.createGroup(1);
	// A truncated timestamp varint.
	malformed.writeFrame({ payload: new Uint8Array([0x80]), timestamp: wallClock() });
	malformed.finish();
	writeGroupWithLegacyFrames(track, 2, [0 as Micro, 100_000 as Micro], wallClock);
	track.finish();
	track.close();

	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(10_000_000 as Micro);
	await expect(nextFrame(consumer)).rejects.toThrow("buffer too short");
	consumer.close();
});

// One payload can decode to several samples (a CMAF fragment). A latency skip that drops the group
// still checks every sample in it, not only those before the one that made the skip eligible.
test("Consumer checks a whole decoded batch before a skip drops its group", async () => {
	const decoded = new Map<number, () => void>();
	const decodes = (timestamp: number) => new Promise<void>((resolve) => decoded.set(timestamp, resolve));
	const later = decodes(3_000_000);
	const rewound = decodes(0);
	// Each payload is a JSON array of sample timestamps.
	const batchFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			const timestamps = JSON.parse(new TextDecoder().decode(frame)) as number[];
			for (const timestamp of timestamps) decoded.get(timestamp)?.();
			return timestamps.map((timestamp) => ({ payload: frame, timestamp: timestamp as Micro, keyframe: true }));
		},
	};
	const write = (sequence: number, timestamps: number[]) => {
		const group = track.createGroup(sequence);
		// Transport stamps follow the sequence, so a lower group may land after a higher one.
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify(timestamps)),
			timestamp: Time.Timestamp.fromMicros(sequence * 1000),
		});
		return group;
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: batchFormat, maxDelay: 500 as Milli });

	write(0, [1_000_000]).finish();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(1_000_000 as Micro);

	// The newer group arrives first and stays open, so the older one is the oldest buffered group
	// and too old to wait for.
	const newest = write(3, [3_000_000]);
	await later;
	// Its first sample is fine; the second rewinds below the delivered group's start.
	write(2, [2_000_000, 0]).finish();
	// Read only once the skip has dropped it, before the newer group's frame is delivered.
	await rewound;
	newest.finish();
	track.finish();
	track.close();

	const read = async () => {
		while (await consumer.next());
	};
	await expect(read()).rejects.toThrow("below the previous group start");
	consumer.close();
});

test("Consumer aborts a rewind carried by a later arrival", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [10_000_000 as Micro], wallClock);
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(10_000_000 as Micro);

	const pending = nextFrame(consumer);
	await settle();
	writeGroupWithLegacyFrames(track, 1, [0 as Micro, 100_000 as Micro], wallClock);

	await expect(pending).rejects.toThrow("below the previous group start");

	consumer.close();
});

// Decode order dips below presentation order inside every group with B-frames. That is not a
// rewind, so the detector compares against the previous group's start.
test("Consumer treats B-frame reordering within a group as continuous", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 66_000 as Micro, 33_000 as Micro]);
	await settle();
	for (const timestamp of [0, 66_000, 33_000]) {
		expect(frameOf(await consumer.next()).timestamp).toBe(timestamp as Micro);
	}

	// The cursor is now on group 1, whose own frames reorder the same way.
	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro, 166_000 as Micro, 133_000 as Micro]);
	await settle();
	for (const timestamp of [100_000, 166_000, 133_000]) {
		// Read directly: a gap here would fail the frame check.
		expect(frameOf(await consumer.next()).timestamp).toBe(timestamp as Micro);
	}

	consumer.close();
});

test("Consumer accepts open-GOP leading pictures above the previous group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 30_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Micro, 50_000 as Micro]);
	await settle();
	for (const timestamp of [0, 33_000, 66_000, 50_000]) {
		expect(frameOf(await consumer.next()).timestamp).toBe(timestamp as Micro);
	}

	consumer.close();
});

// A delivered group still awaiting its done result holds no unread media, so retiring it for
// the max delay loses nothing.
test("Consumer reads no gap when retiring a fully delivered group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 150 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);

	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Micro]);
	await settle();

	// Read directly: a gap here would fail the frame check.
	expect(frameOf(await consumer.next()).timestamp).toBe(100_000 as Micro);

	consumer.close();
});

// An unread trailing endpoint carries no media, so retiring its group loses nothing.
test("Consumer reads no gap when retiring a group holding only its endpoint", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("video"), maxDelay: 150 as Milli });

	const group = track.createGroup(0);
	group.writeFrame({ payload: encodeLegacy(0 as Micro), timestamp: Time.Timestamp.fromMicros(0) });
	group.writeFrame({
		payload: encodeLegacyFrame(33_000 as Micro, new Uint8Array()),
		timestamp: Time.Timestamp.fromMicros(33_000),
	});
	group.finish();
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);

	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Micro]);
	await settle();

	expect(frameOf(await consumer.next()).timestamp).toBe(100_000 as Micro);

	consumer.close();
});

// A truncated group lost its tail, so retiring it for the max delay is a gap even when the
// next group is adjacent.
test("Consumer reads a gap when retiring a truncated group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 150 as Milli });

	const group = track.createGroup(0);
	group.writeFrame({ payload: encodeLegacy(0 as Micro), timestamp: Time.Timestamp.fromMicros(0) });
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);

	group.abort(new NetError.Stream(StreamCode.Cancel));
	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Micro]);
	await settle();

	// Legacy carries no duration, so the gap starts at the last frame read.
	expect(await consumer.next()).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 100_000 as Micro });
	expect(frameOf(await consumer.next()).timestamp).toBe(100_000 as Micro);

	consumer.close();
});

// An open group may still have a tail in flight, so adjacency proves nothing when it is retired.
test("Consumer reads a gap when retiring an open group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 150 as Milli });

	const group = track.createGroup(0);
	group.writeFrame({ payload: encodeLegacy(0 as Micro), timestamp: Time.Timestamp.fromMicros(0) });
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);

	writeGroupWithLegacyFrames(track, 1, [100_000 as Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Micro]);
	await settle();

	// Legacy carries no duration, so the gap starts at the last frame read.
	expect(await consumer.next()).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 100_000 as Micro });
	expect(frameOf(await consumer.next()).timestamp).toBe(100_000 as Micro);

	group.finish();
	consumer.close();
});

// --- Buffered signal ---

test("Consumer buffered signal updates as frames arrive", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 500 as Milli });

	expect(consumer.buffered.peek()).toEqual([]);

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Micro, 99_000 as Micro]);

	await sleep(Nano.fromMilli(Milli(100)));

	const ranges = consumer.buffered.peek();
	expect(ranges.length).toBe(1);
	expect(ranges[0].start).toBe(0 as Milli);
	expect((ranges[0].end as number) >= 66).toBeTruthy();

	track.finish();
	track.close();
	consumer.close();
});

// --- Gap recovery ---

test("Consumer recovers from gap in group sequence numbers", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 100 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 20_000 as Micro]);
	writeGroupWithLegacyFrames(track, 1, [40_000 as Micro, 60_000 as Micro]);
	// Skip group 2
	writeGroupWithLegacyFrames(track, 3, [120_000 as Micro, 140_000 as Micro]);
	writeGroupWithLegacyFrames(track, 4, [160_000 as Micro, 180_000 as Micro]);
	writeGroupWithLegacyFrames(track, 5, [200_000 as Micro, 220_000 as Micro]);
	track.finish();
	track.close();

	await sleep(Nano.fromMilli(Milli(100)));

	const frames = await drainFrames(consumer, 500);
	expect(frames.length >= 4).toBeTruthy();
	consumer.close();
});

// --- Edge cases from design review ---

test("Consumer handles empty decode result without deadlock", async () => {
	let callCount = 0;
	const emptyThenValid: ContainerFormat = {
		decode(_frame: Uint8Array): Frame[] {
			callCount++;
			if (callCount === 1) return []; // empty result
			return [{ payload: new Uint8Array([1]), timestamp: 33_000 as Micro, keyframe: false }];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: emptyThenValid, maxDelay: 500 as Milli });

	const group = track.createGroup(0);
	group.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() }); // empty decode
	group.writeFrame({ payload: new Uint8Array([0x02]), timestamp: Time.Timestamp.now() }); // valid decode
	group.finish();
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 300);
	// The empty decode produces no frames, but the second MoQ frame does.
	// Since index 0 was never used (empty result), the first actual frame gets index=1 → keyframe false?
	// Actually index increments per sample, and empty decode means 0 samples → index stays at 0.
	// So the next frame's first sample gets index=0 → keyframe=true.
	expect(frames).toHaveLength(1);
	expect(frames[0].keyframe).toBe(true);
	consumer.close();
});

test("Consumer preserves empty media from formats without endpoint markers", async () => {
	const format: ContainerFormat = {
		decode(): Frame[] {
			return [{ payload: new Uint8Array(), timestamp: 0 as Micro, keyframe: false }];
		},
	};
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format, maxDelay: 500 as Milli });
	const group = track.createGroup(0);
	group.writeFrame({ payload: new Uint8Array([1]), timestamp: Time.Timestamp.now() });
	group.finish();
	track.finish();
	track.close();

	expect(frameOf(await nextFrame(consumer)).payload).toHaveLength(0);
	consumer.close();
});

// --- CMAF through Consumer ---

test("Consumer with CmafFormat delivers correct timestamps", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxDelay: 500 as Milli,
	});

	const group = track.createGroup(0);
	group.writeFrame({
		payload: fragment("key"),
		timestamp: at(0),
	});
	group.writeFrame({
		payload: fragment("delta"),
		timestamp: at(3000),
	});
	group.finish();
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(2);
	expect(frames[0].keyframe).toBe(true); // index 0 override
	expect(frames[1].keyframe).toBe(false); // trusts format
	expect(frames[0].timestamp).toBe(0 as Micro);
	expect(frames[1].timestamp).toBe(33_333 as Micro); // 3000/90000 * 1_000_000
	consumer.close();
});

test("Consumer with CmafFormat times an untimed track from tfdt", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxDelay: 500 as Milli,
	});

	const group = track.createGroup(0);
	group.writeFrame({ payload: fragment("untimedKey") });
	group.writeFrame({ payload: fragment("untimedDelta") });
	group.finish();
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames.map((f) => f.timestamp)).toEqual([1_000_000, 1_033_333] as Micro[]);
	consumer.close();
});

test("CmafFormat decodes the per-sample duration", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = fragment("key");

	const [frame] = format.decode(segment, at(0));
	// 3000 ticks / 90000 timescale * 1_000_000 = 33333µs
	expect(frame.duration).toBe(33_333 as Micro);
});

// --- Duration skipping ---

// Format whose frames carry a fixed 33ms duration; the timestamp is byte 0 (ms).
const durationFormat: ContainerFormat = {
	decode(frame: Uint8Array): Frame[] {
		return [
			{
				payload: frame,
				timestamp: (frame[0] * 1000) as Micro,
				duration: 33_000 as Micro,
				keyframe: false,
			},
		];
	},
};

test("Consumer duration-skips a stalled group once it is covered", async () => {
	const track = new Track.Producer("test");
	// Latency dwarfs the gap, so only duration coverage can trigger the skip.
	const consumer = new Consumer(replay(track), { format: durationFormat, maxDelay: 10_000 as Milli });

	// Group.Producer 0: one frame at ts=0 lasting 33ms, never closed (stalled).
	const g0 = track.createGroup(0);
	g0.writeFrame({ payload: new Uint8Array([0]), timestamp: Time.Timestamp.now() });

	// Group.Producer 1: closed, starts exactly where group 0's frame ends.
	const g1 = track.createGroup(1);
	g1.writeFrame({ payload: new Uint8Array([33]), timestamp: Time.Timestamp.now() });
	g1.finish();

	track.finish();

	const frames = await drainFrames(consumer, 200);
	expect(frames.map((f) => f.timestamp as number)).toEqual([0, 33_000]);
	expect(frames.map((f) => f.group)).toEqual([0, 1]);
	consumer.close();
	track.close();
});

test("Consumer does not duration-skip when the gap is not covered", async () => {
	// Format whose frames last only 10ms, short of the 33ms gap to the next group.
	const shortFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			return [
				{
					payload: frame,
					timestamp: (frame[0] * 1000) as Micro,
					duration: 10_000 as Micro,
					keyframe: false,
				},
			];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: shortFormat, maxDelay: 10_000 as Milli });

	// Group.Producer 0 stays open and later receives a second frame; nothing covers the gap,
	// so that late frame must survive rather than being skipped.
	const g0 = track.createGroup(0);
	g0.writeFrame({ payload: new Uint8Array([0]), timestamp: Time.Timestamp.now() });

	const g1 = track.createGroup(1);
	g1.writeFrame({ payload: new Uint8Array([33]), timestamp: Time.Timestamp.now() });
	g1.finish();

	// Let the consumer settle on group 0, then extend it before closing.
	await sleep(Nano.fromMilli(Milli(20)));
	g0.writeFrame({ payload: new Uint8Array([20]), timestamp: Time.Timestamp.now() });
	g0.finish();
	track.finish();
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames.map((f) => f.timestamp as number)).toEqual([0, 20_000, 33_000]);
	expect(frames.map((f) => f.group)).toEqual([0, 0, 1]);
	consumer.close();
});

// --- Non-sequential group ids: PTS-timeline-gated incremental delivery (regression) ---

// Group numbers may be non-sequential (large, non-+1 jumps). Delivery follows the PTS *timeline*,
// not the numbering: a next group whose first frame continues where the active group ended must
// surface immediately, even while still open.
test("Consumer delivers a PTS-contiguous next group whose sequence jumped (Legacy)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 500 as Milli,
	});

	// Group A (seq 1_000_000): one 3000-tick sample, so its content ends at 33_333µs (3000/90000 * 1e6).
	const a = track.createGroup(1_000_000);
	a.writeFrame({
		payload: encodeLegacyFrame(Math.round((0 / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([0x01])),
		timestamp: at(0),
	});
	a.finish();

	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));

	const pending = consumer.next();

	// Group B: sequence jumps +90_000, and its first PTS (3045 ticks ~= 33_833µs) sits ~500µs past
	// A.end (33_333µs) -- the sub-millisecond skew a genuinely contiguous CMAF boundary shows because
	// per-sample durations and base-decode-times round to µs independently. It's within the contiguity
	// tolerance, so the timeline is unbroken and B must deliver immediately even while still open.
	const b = track.createGroup(1_090_000);
	b.writeFrame({
		payload: encodeLegacyFrame(Math.round((3045 / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([0x02])),
		timestamp: at(3045),
	});

	const result = await Promise.race([pending, sleep(Nano.fromMilli(Milli(500))).then(() => "timeout" as const)]);
	expect(result).not.toBe("timeout");
	expect(frameOf(result).payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// A real PTS gap (the active group's end does NOT reach the next buffered group's first frame) means
// an intermediate group may still be in transit, so we must NOT skip ahead and wreck ordering.
// Delivery resumes only once the missing, timeline-continuous group arrives.
test("Consumer waits on a PTS gap instead of skipping to a later buffered group (Legacy)", async () => {
	const track = new Track.Producer("test");
	// Large max delay so the gap can't be age-skipped during the test window.
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 10_000 as Milli,
	});

	// Group A (seq 1_000_000): content ends at 33_333µs.
	const a = track.createGroup(1_000_000);
	a.writeFrame({
		payload: encodeLegacyFrame(Math.round((0 / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([0x01])),
		timestamp: at(0),
	});
	a.finish();

	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));

	const pending = consumer.next();

	// Group C (seq 1_090_000) starts at 90_000 ticks (1_000_000µs) -- far past A.end (33_333µs), a real
	// gap. An intermediate group may still be in transit, so C must NOT be delivered yet.
	const c = track.createGroup(1_090_000);
	c.writeFrame({
		payload: encodeLegacyFrame(Math.round((90_000 / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([0x03])),
		timestamp: at(90_000),
	});

	const gap = await Promise.race([pending, sleep(Nano.fromMilli(Milli(300))).then(() => "timeout" as const)]);
	expect(gap).toBe("timeout"); // held: the gap is not eagerly skipped

	// The missing group B arrives, contiguous with A (first PTS 3000 ticks = 33_333µs = A.end); it
	// continues the timeline, so delivery resumes with B's frame.
	const b = track.createGroup(1_045_000);
	b.writeFrame({
		payload: encodeLegacyFrame(Math.round((3000 / TIMESCALE) * 1_000_000) as Micro, new Uint8Array([0x02])),
		timestamp: at(3000),
	});

	const result = await pending;
	expect(frameOf(result).payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// A group can finish before it becomes the active one (it arrived and completed while an earlier
// group was still open). The cursor then moves past it in next() rather than in #runGroup, so that
// is where its presentation end has to be recorded. Miss it and the stale end from the group before
// blocks every later contiguous group.
test("Consumer delivers a contiguous group after one that completed out of order (Legacy)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 10_000 as Milli,
	});

	// A (seq 1_000_000) stays open so it remains the active group.
	const a = track.createGroup(1_000_000);
	a.writeFrame(timedFrame(0x01, 0, 0));

	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));

	// A's second frame stays buffered so A can't be duration-skipped. A ends at 6000 ticks (66_666µs).
	a.writeFrame(timedFrame(0x02, 3000, 1));
	await settle();

	// B (seq 1_045_000) starts at A's end and is CLOSED while A is still active, so B's own finally
	// block sees sequence !== #active and never records anything. B ends at 9000 ticks (100_000µs).
	const b = track.createGroup(1_045_000);
	b.writeFrame(timedFrame(0x03, 6000, 2));
	b.finish();
	await settle();

	a.finish();
	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0x02]));
	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0x03]));

	// C (seq 1_090_000) starts at B's end, so it continues the timeline and must deliver.
	const pending = consumer.next();
	const c = track.createGroup(1_090_000);
	c.writeFrame(timedFrame(0x04, 9000, 3));

	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).not.toBe("timeout");
	expect(frameOf(result).payload).toEqual(new Uint8Array([0x04]));

	consumer.close();
});

// A resubscribe can replay a stale group, lose the group after it, then carry on live. Once the live
// head reaches past where presentation left off by more than the budget, whatever is missing would
// arrive too old to play, so the head is the group to play next and must not be dropped as "slow".
test("Consumer plays the head once a waited-out gap exceeds the budget (Legacy)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 100 as Milli,
	});

	// A (seq 1000): one frame, ends at 3000 ticks (33_333µs).
	const a = track.createGroup(1000);
	a.writeFrame(timedFrame(0x01, 0, 0));
	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));
	a.finish();

	// The reader is parked on the missing 1001, as a decoder is while it waits for the next group.
	const pending = consumer.next();

	// B (seq 2000) starts at 90_000 ticks (1_000_000µs), a second past A's end: the missing group
	// could only ever arrive far beyond the 100ms budget, so B plays from its first frame.
	const b = track.createGroup(2000);
	b.writeFrame(timedFrame(0x02, 90_000, 1));

	// The missing group's span was never delivered, so a gap from A's end to B comes first.
	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).toEqual({ kind: "gap", timestamp: 33_333 as Micro, duration: 966_667 as Micro });
	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// A gap still inside the budget is waited out: the missing group may yet arrive in time.
test("Consumer waits on a gap within the budget, then plays the head once it exceeds it (Legacy)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 100 as Milli,
	});

	// A (seq 1000): one frame, ends at 3000 ticks (33_333µs).
	const a = track.createGroup(1000);
	a.writeFrame(timedFrame(0x01, 0, 0));
	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));
	a.finish();

	// B (seq 2000) starts at 6000 ticks (66_667µs): a gap, but only 33ms past A's end.
	const b = track.createGroup(2000);
	b.writeFrame(timedFrame(0x02, 6000, 1));

	const pending = consumer.next();
	const early = await Promise.race([pending, settle(50).then(() => "waiting" as const)]);
	expect(early).toBe("waiting");

	// B alone grows past the budget (up to 12_000 ticks, 100ms past A's end and beyond).
	for (let i = 1; i <= 3; i++) {
		b.writeFrame(timedFrame(0x02, 6000 + i * 3000, 1 + i));
		await settle(10);
	}

	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).toEqual({ kind: "gap", timestamp: 33_333 as Micro, duration: 33_334 as Micro });
	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// --- Gaps and ends ---

// A gap is what downstream buffer accounting keys on, so it must appear exactly when the consumer
// dropped something. Group numbers can't answer that: they aren't required to be sequential, and
// adjacency doesn't rule out a group the max delay check dropped on the way past.
test("Consumer yields no gap while nothing is dropped", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 100 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro, 33_000 as Micro]);
	await settle();

	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);
	expect(frameOf(await consumer.next()).timestamp).toBe(33_000 as Micro);

	writeGroupWithLegacyFrames(track, 1, [66_000 as Micro]);
	await settle();
	expect(frameOf(await consumer.next()).timestamp).toBe(66_000 as Micro); // group 1 continues group 0

	consumer.close();
});

test("Consumer reads a marker group as an end, not a gap", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 2_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	writeMarkerGroup(track, 1, 0 as Micro);
	writeGroupWithLegacyFrames(track, 2, [1_000_000 as Micro]);
	await settle();

	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);
	expect(await consumer.next()).toEqual({ kind: "end", group: 1, end: 0 as Micro });
	// A declared stop loses nothing, so no gap follows it.
	expect(frameOf(await consumer.next()).timestamp).toBe(1_000_000 as Micro);

	consumer.close();
});

test("Consumer empty groups mean nothing", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 2_000 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	const empty = track.createGroup(1);
	empty.finish();
	writeGroupWithLegacyFrames(track, 2, [1_000 as Micro]);
	await settle();

	expect(frameOf(await consumer.next()).timestamp).toBe(0 as Micro);
	expect(frameOf(await consumer.next()).timestamp).toBe(1_000 as Micro);

	consumer.close();
});

test("Consumer reads a latency skip as one gap", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 0 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Micro);

	writeGroupWithLegacyFrames(track, 2, [100_000 as Micro]);
	await settle();

	expect(await consumer.next()).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 100_000 as Micro });
	expect(frameOf(await consumer.next()).timestamp).toBe(100_000 as Micro);

	consumer.close();
});

test("Consumer reads a timestamp hole as a gap", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 0 as Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Micro);

	writeGroupWithLegacyFrames(track, 3, [1_000_000 as Micro]);
	await settle();

	expect(await consumer.next()).toEqual({
		kind: "gap",
		timestamp: 0 as Micro,
		duration: 1_000_000 as Micro,
	});
	expect(frameOf(await consumer.next()).timestamp).toBe(1_000_000 as Micro);

	consumer.close();
});

test("Consumer reads a shed marker group as a gap", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxDelay: 0 as Milli });

	const group0 = track.createGroup(0);
	group0.writeFrame({
		payload: encodeLegacy(0 as Micro),
		timestamp: Time.Timestamp.fromMicros(0),
	});
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Micro);

	writeMarkerGroup(track, 1, 0 as Micro);
	writeGroupWithLegacyFrames(track, 2, [500 as Micro]);
	await settle();

	// The zero budget sheds the marker unread, and what it declared is lost with it.
	expect(await consumer.next()).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 500 as Micro });
	expect(frameOf(await consumer.next()).timestamp).toBe(500 as Micro);
	group0.finish();

	consumer.close();
});

// The case the group-number heuristic got backwards: ids jump, but the PTS timeline is unbroken.
test("Consumer yields no gap across a PTS-contiguous group id jump (Legacy)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: timedLegacy,
		maxDelay: 10_000 as Milli,
	});

	const a = track.createGroup(1_000_000);
	a.writeFrame(timedFrame(0x01, 0, 0));
	a.finish();

	expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([0x01]));

	// Sequence jumps +90_000 but the first PTS meets A's end, so nothing is missing.
	const b = track.createGroup(1_090_000);
	b.writeFrame(timedFrame(0x02, 3000, 1));

	expect(frameOf(await consumer.next()).payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

test("a live video frame is delivered before its duration marker arrives", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("video"),
		maxDelay: 500 as Milli,
	});
	const group = track.appendGroup();
	group.writeFrame({
		payload: encodeLegacyFrame(0 as Micro, new Uint8Array([1])),
		timestamp: Time.Timestamp.now(),
	});
	let delivered = false;
	const read = consumer.next().then((value) => {
		delivered = true;
		return value;
	});
	await settle();
	expect(delivered).toBe(true);
	expect(frameOf(await read).timestamp).toBe(0 as Micro);
	group.writeFrame({
		payload: encodeLegacyFrame(15_000 as Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	group.finish();
	track.finish();
	track.close();
	// The marker is the frame's duration, not an item.
	expect(await consumer.next()).toBeUndefined();
	consumer.close();
	track.finish();
	track.close();
});

test("audio discontinuity writes no duration marker", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe();
	const producer = new LegacyProducer(track, new LegacyFormat("audio"));
	producer.encode(new Uint8Array([1]), 0 as Micro, true);
	producer.discontinuity(15_000 as Micro);
	const group = await subscriber.recvGroup();
	expect(await group?.readFrame()).toBeDefined();
	expect(await group?.readFrame()).toBeUndefined();
	producer.close();
});

for (const code of [StreamCode.Cancel, StreamCode.Internal, StreamCode.Old, StreamCode.Evicted, StreamCode(1234)]) {
	test(`Consumer preserves frames and continues after group reset ${code}`, async () => {
		const track = new Track.Producer("test");
		const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxDelay: 0 as Milli });
		const error = spyOn(console, "error").mockImplementation(() => {});
		try {
			const group = track.appendGroup();
			group.writeFrame({
				payload: encodeLegacyFrame(0 as Micro, new Uint8Array([1])),
				timestamp: Time.Timestamp.now(),
			});
			await settle();
			group.abort(new NetError.Stream(code));
			await settle();
			expect(frameOf(await nextFrame(consumer)).payload).toEqual(new Uint8Array([1]));
			const next = track.appendGroup();
			next.writeFrame({
				payload: encodeLegacyFrame(1 as Micro, new Uint8Array([2])),
				timestamp: Time.Timestamp.now(),
			});
			next.finish();
			// The reset cut the group's tail.
			expect(await consumer.next()).toEqual({
				kind: "gap",
				timestamp: 0 as Micro,
				duration: 1 as Micro,
			});
			const resumed = frameOf(await consumer.next());
			expect(resumed.payload).toEqual(new Uint8Array([2]));
			expect(resumed.keyframe).toBe(true);
			expect(error).not.toHaveBeenCalled();
		} finally {
			consumer.close();
			track.finish();
			track.close();
			error.mockRestore();
		}
	});
}

for (const end of [
	null,
	new NetError.Stream(StreamCode.Cancel),
	new NetError.Stream(StreamCode.Internal),
	new NetError.Session(SessionCode.ProtocolViolation),
]) {
	test(`Consumer settles a pending read when the track ends: ${end}`, async () => {
		const track = new Track.Producer("test");
		const consumer = new Consumer(replay(track), { format: new LegacyFormat("data") });
		try {
			const pending = consumer.next();
			if (end) track.abort(end);
			else {
				track.finish();
				track.close();
			}
			if (end) await expect(pending).rejects.toBe(end);
			else expect(await pending).toBeUndefined();
		} finally {
			consumer.close();
		}
	});
}

test("a group reset does not hide a later container decode failure", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data") });
	try {
		const reset = track.appendGroup();
		await settle();
		reset.abort(new NetError.Stream(StreamCode.Cancel));
		await settle();
		const malformed = track.appendGroup();
		// A truncated timestamp varint.
		malformed.writeFrame({ payload: new Uint8Array([0x80]), timestamp: Time.Timestamp.now() });
		malformed.finish();
		track.finish();
		track.close();
		await expect(nextFrame(consumer)).rejects.toThrow("buffer too short");
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});

for (const end of [
	null,
	new NetError.Stream(StreamCode.Internal),
	new NetError.Session(SessionCode.ProtocolViolation),
]) {
	test(`Consumer drains a permanent buffered gap after track termination: ${end}`, async () => {
		const track = new Track.Producer("test");
		const consumer = new Consumer(replay(track), {
			format: new LegacyFormat("data"),
			maxDelay: 10_000 as Milli,
		});
		try {
			writeGroupWithLegacyFrames(track, 0, [0 as Micro]);
			expect(await nextFrame(consumer)).toMatchObject({ group: 0 });
			writeGroupWithLegacyFrames(track, 2, [1_000_000 as Micro]);
			await settle();
			const pending = consumer.next();
			if (end) track.abort(end);
			else {
				track.finish();
				track.close();
			}
			const resumed = await Promise.race([pending, settle(100).then(() => "stalled" as const)]);
			expect(resumed).not.toBe("stalled");
			if (resumed === "stalled") return;
			expect(resumed).toEqual({ kind: "gap", timestamp: 0 as Micro, duration: 1_000_000 as Micro });
			expect(await consumer.next()).toMatchObject({ kind: "frame", group: 2, frame: { timestamp: 1_000_000 } });
			if (end) await expect(consumer.next()).rejects.toBe(end);
			else expect(await consumer.next()).toBeUndefined();
		} finally {
			consumer.close();
			track.finish();
			track.close();
		}
	});
}

test("LegacyFormat rejects a timestamp past 2^53 - 1 instead of rounding", () => {
	const frame = Varint.encode(2n ** 53n + 1n);
	expect(() => new LegacyFormat("video").decode(frame)).toThrow(/larger than 53-bits/);
});
