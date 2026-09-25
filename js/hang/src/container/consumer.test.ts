import { expect, spyOn, test } from "bun:test";
import { Format as LocFormat, Producer as LocProducer } from "@moq/loc";
import { Group, Error as NetError, SessionCode, StreamCode, Time, Track, Varint } from "@moq/net";
import { AudioConfigSchema } from "../catalog/audio.ts";
import { decodeInitSegment, type InitSegment } from "./cmaf/decode.ts";
import { createAudioInitSegment, encodeDataSegment } from "./cmaf/encode.ts";
import { Format as CmafFormat } from "./cmaf/format.ts";
import { Consumer } from "./consumer.ts";
import type { Format as ContainerFormat } from "./format.ts";
import { Format as LegacyFormat, Producer as LegacyProducer } from "./legacy.ts";
import type { Frame } from "./types.ts";

const TIMESCALE = 90_000;
const TEST_INIT: InitSegment = {
	timescale: TIMESCALE,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 0,
	defaultSampleSize: 0,
	defaultSampleFlags: 0,
};

function encodeLegacyFrame(timestamp: Time.Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

/** A one-byte CMAF sample at `timestamp` ticks, lasting one 3000-tick (33_333µs) frame. */
function encodeCmafFrame(data: number, timestamp: number, sequence: number): Uint8Array {
	return encodeDataSegment({
		kind: "video",
		data: new Uint8Array([data]),
		timestamp,
		duration: 3000,
		keyframe: true,
		sequence,
	});
}

/** Yield long enough for the consumer's spawned group readers to drain what's been written. */
function settle(ms = 20): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

// Keep transport drift filtering out of container-consumer tests. These cases
// exercise the consumer's own max age policy against a complete retained track.
// These tests write every group up front and only then read, so they ask for history
// rather than the live edge.
function replay(track: Track.Producer): Track.Subscriber {
	return track.subscribe({ maxAge: Time.Milli(30_000) });
}

// --- LegacyFormat ---

test("LegacyFormat decodes a valid frame", () => {
	const format = new LegacyFormat("data");
	const payload = new Uint8Array([0xde, 0xad]);
	const timestamp = 1000 as Time.Micro;
	const frame = encodeLegacyFrame(timestamp, payload);

	const result = format.decode(frame);

	expect(result).toHaveLength(1);
	expect(result[0].timestamp).toBe(timestamp);
	expect(result[0].payload).toEqual(payload);
	expect(result[0].keyframe).toBe(false);
});

test("LegacyFormat preserves a duration marker", () => {
	const format = new LegacyFormat("video");
	const frame = encodeLegacyFrame(1000 as Time.Micro, new Uint8Array());

	const [marker] = format.decode(frame);
	expect(marker.timestamp).toBe(1000 as Time.Micro);
	expect(marker.payload).toHaveLength(0);
	expect(format.end(marker)).toBe(1000 as Time.Micro);
});

test("LegacyFormat always returns keyframe: false", () => {
	const format = new LegacyFormat("data");
	const frame = encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0x01]));

	const [decoded] = format.decode(frame);
	expect(decoded.keyframe).toBe(false);
});

test("LegacyFormat always returns exactly one frame", () => {
	const format = new LegacyFormat("data");
	const frame = encodeLegacyFrame(5000 as Time.Micro, new Uint8Array([0x01, 0x02, 0x03]));

	const result = format.decode(frame);
	expect(result).toHaveLength(1);
});

test("LegacyFormat throws on empty input", () => {
	const format = new LegacyFormat("data");
	expect(() => format.decode(new Uint8Array(0))).toThrow();
});

test("Legacy Producer refuses a group below the live edge", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Time.Micro, false);
	expect(() => producer.encode(new Uint8Array([1]), 16_000 as Time.Micro, true)).toThrow("below the live edge");
	producer.close();
});

test("Legacy Producer accepts B-frames within a group", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 66_000 as Time.Micro, false);
	producer.encode(new Uint8Array([1]), 33_000 as Time.Micro, false);
	producer.close();
});

test("Legacy Producer accepts open-GOP leading pictures above the previous group", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Time.Micro, false);
	producer.encode(new Uint8Array([1]), 66_000 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 50_000 as Time.Micro, false);
	producer.close();
});

test("Legacy Producer writes a duration marker at the next keyframe", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxAge: Time.Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([0xde, 0xad]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([0xbe, 0xef]), 10_000 as Time.Micro, false);
	producer.encode(new Uint8Array([0xca, 0xfe]), 20_000 as Time.Micro, true);
	producer.close();

	const group = await subscriber.recvGroup();
	expect(group).toBeDefined();
	const frames: { timestamp: Time.Micro; size: number }[] = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) break;
		const [timestamp, payload] = Varint.decode(frame.payload);
		frames.push({ timestamp: timestamp as Time.Micro, size: payload.byteLength });
	}
	expect(frames).toEqual([
		{ timestamp: 0 as Time.Micro, size: 2 },
		{ timestamp: 10_000 as Time.Micro, size: 2 },
		{ timestamp: 20_000 as Time.Micro, size: 0 },
	]);
});

test("Legacy Producer omits a reordered group's presentation endpoint marker", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxAge: Time.Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	for (const [index, timestamp] of [0, 120_000, 40_000, 80_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Time.Micro, index === 0);
	}
	producer.encode(new Uint8Array([1]), 160_000 as Time.Micro, true);
	producer.cut(200_000 as Time.Micro);
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
	const subscriber = track.subscribe({ maxAge: Time.Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	for (const [index, timestamp] of [0, 16_000, 32_000, 65_000, 98_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Time.Micro, index === 0);
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

test("Legacy Producer rejects a backwards cut without closing the group", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe({ maxAge: Time.Milli(30_000) });
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Time.Micro, true);
	expect(() => producer.cut(10_000 as Time.Micro)).toThrow();
	producer.encode(new Uint8Array([2]), 30_000 as Time.Micro, false);
	producer.cut(35_000 as Time.Micro);
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

test("Legacy Producer refuses a keyframe that rewinds the timeline", () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 20_000 as Time.Micro, true);
	producer.encode(new Uint8Array([2]), 30_000 as Time.Micro, false);
	expect(() => producer.encode(new Uint8Array([3]), 0 as Time.Micro, true)).toThrow("below the live edge");
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

test("Legacy Producer cut marks the break with one empty frame at the live edge", async () => {
	const track = new Track.Producer("test");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("audio"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 20_000 as Time.Micro, true);
	producer.cut();
	producer.cut(); // nothing new to mark
	producer.encode(new Uint8Array([1]), 5_000_000 as Time.Micro, true);
	producer.close();

	expect(await readGroups(subscriber, 3)).toEqual([
		[0, [[0, 1]]],
		[1, [[20_000, 1]]],
		[2, [[20_000, 0]]],
		[3, [[5_000_000, 1]]],
	]);
});

test("Legacy Producer cut marks the break at the caller's end", async () => {
	const track = new Track.Producer("test");
	const subscriber = replay(track);
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.cut(33_000 as Time.Micro);
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

test("Legacy Producer cut marks nothing on a data track or before any frame", async () => {
	const data = new Track.Producer("data");
	const producer = new LegacyProducer(data, new LegacyFormat("data"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.cut();
	expect(data.appendGroup().sequence).toBe(1);

	const empty = new Track.Producer("empty");
	new LegacyProducer(empty, new LegacyFormat("video")).cut();
	expect(empty.appendGroup().sequence).toBe(0);
});

// A group's reach runs to its successor's first frame, so without the marker the group before a
// pause would stretch across the whole gap and read as live to anyone joining after the resume.
test("Legacy Producer cut keeps pre-pause media from reading as live", async () => {
	const track = new Track.Producer("test");
	const producer = new LegacyProducer(track, new LegacyFormat("video"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 33_000 as Time.Micro, false);
	producer.cut();
	producer.encode(new Uint8Array([1]), 5_000_000 as Time.Micro, true);
	producer.close();

	const subscriber = track.subscribe({ maxAge: Time.Milli(1_000) });
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
	const segment = encodeDataSegment({
		kind: "video",
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);

	expect(result).toHaveLength(1);
	expect(result[0].payload).toEqual(new Uint8Array([0xca, 0xfe]));
	expect(result[0].timestamp).toBe(0 as Time.Micro);
	expect(result[0].keyframe).toBe(true);
});

// Packagers flag every audio sample a sync sample. Reported as a keyframe, each would open
// its own group, so audio never decodes one: the consumer marks the group start instead.
test("CmafFormat never reports an audio keyframe", () => {
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48000,
		numberOfChannels: 2,
	});
	const init = decodeInitSegment(createAudioInitSegment(config));
	expect(init.kind).toBe("audio");

	const format = new CmafFormat(init);
	const segment = encodeDataSegment({
		kind: "audio",
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 20000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);
	expect(result).toHaveLength(1);
	expect(result[0].keyframe).toBe(false);
});

test("CMAF init rejects an unsupported track handler", () => {
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48000,
		numberOfChannels: 2,
	});
	const init = createAudioInitSegment(config);
	const offset = Buffer.from(init).indexOf("soun");
	expect(offset).toBeGreaterThanOrEqual(0);
	init.set(new TextEncoder().encode("text"), offset);
	expect(() => decodeInitSegment(init)).toThrow("Unsupported track handler: text");
});

test("CmafFormat decodes a delta frame segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		kind: "video",
		data: new Uint8Array([0xbe, 0xef]),
		timestamp: 3000,
		duration: 3000,
		keyframe: false,
		sequence: 1,
	});

	const result = format.decode(segment);

	expect(result).toHaveLength(1);
	expect(result[0].keyframe).toBe(false);
});

test("CmafFormat converts timescale units to microseconds", () => {
	const format = new CmafFormat(TEST_INIT);
	// 90000 timescale units = 1 second = 1_000_000 microseconds
	const segment = encodeDataSegment({
		kind: "video",
		data: new Uint8Array([0x01]),
		timestamp: TIMESCALE,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);
	expect(result[0].timestamp).toBe(1_000_000 as Time.Micro);
});

test("CmafFormat throws on corrupt segment", () => {
	const format = new CmafFormat(TEST_INIT);
	expect(() => format.decode(new Uint8Array([0x00, 0x01, 0x02]))).toThrow();
});

// --- Consumer ---

function encodeLegacy(timestamp: Time.Micro): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const payload = new Uint8Array([0xde, 0xad]);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

function writeGroupWithLegacyFrames(track: Track.Producer, sequence: number, timestamps: Time.Micro[]) {
	const group = new Group.Producer(sequence);
	for (const ts of timestamps) {
		group.writeFrame({ payload: encodeLegacy(ts), timestamp: Time.Timestamp.now() });
	}
	group.close();
	track.writeGroup(group);
}

function writeMarkerGroup(track: Track.Producer, sequence: number, timestamp: Time.Micro) {
	const group = new Group.Producer(sequence);
	group.writeFrame({
		payload: encodeLegacyFrame(timestamp, new Uint8Array()),
		timestamp: Time.Timestamp.fromMicros(timestamp),
	});
	group.close();
	track.writeGroup(group);
}

async function drainFrames(
	consumer: Consumer,
	timeout: number,
): Promise<{ timestamp: Time.Micro; group: number; keyframe: boolean }[]> {
	const frames: { timestamp: Time.Micro; group: number; keyframe: boolean }[] = [];
	for (;;) {
		const result = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), timeout)),
		]);
		if (result === null || result === undefined) break;
		if (result.frame) {
			frames.push({ timestamp: result.frame.timestamp, group: result.group, keyframe: result.frame.keyframe });
		}
	}
	return frames;
}

test("Consumer delivers frames from a single group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(2);
	expect(frames[0].timestamp).toBe(0 as Time.Micro);
	expect(frames[1].timestamp).toBe(33_000 as Time.Micro);
	consumer.close();
});

test("Consumer forces keyframe true at index 0", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
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
				{ payload: new Uint8Array([1]), timestamp: 0 as Time.Micro, keyframe: false },
				{ payload: new Uint8Array([2]), timestamp: 33_000 as Time.Micro, keyframe: false },
				{ payload: new Uint8Array([3]), timestamp: 66_000 as Time.Micro, keyframe: false },
			];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: multiFormat, maxAge: 500 as Time.Milli });

	const group = new Group.Producer(0);
	group.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() }); // first MoQ frame → 3 samples
	group.writeFrame({ payload: new Uint8Array([0x02]), timestamp: Time.Timestamp.now() }); // second MoQ frame → 3 samples
	group.close();
	track.writeGroup(group);
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(6);
	// Only index 0 is keyframe, rest are false
	expect(frames.map((f) => f.keyframe)).toEqual([true, false, false, false, false, false]);
	consumer.close();
});

test("Consumer keeps frames decoded before an error (truncated GoP)", async () => {
	// 0xFF in the first byte signals the format to throw, simulating a stream
	// RESET or corrupt frame mid-group. Encoding the trigger in the frame bytes
	// keeps this deterministic when groups decode in parallel.
	const truncatingFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			if (frame[0] === 0xff) throw new Error("truncated");
			return [{ payload: frame, timestamp: frame[0] as Time.Micro, keyframe: false }];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: truncatingFormat, maxAge: 500 as Time.Milli });

	// Group.Producer 0: 2 valid frames then a tail-truncating error.
	const g0 = new Group.Producer(0);
	g0.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() });
	g0.writeFrame({ payload: new Uint8Array([0x02]), timestamp: Time.Timestamp.now() });
	g0.writeFrame({ payload: new Uint8Array([0xff]), timestamp: Time.Timestamp.now() });
	g0.close();
	track.writeGroup(g0);

	// Group.Producer 1 decodes cleanly.
	const g1 = new Group.Producer(1);
	g1.writeFrame({ payload: new Uint8Array([0x04]), timestamp: Time.Timestamp.now() });
	g1.close();
	track.writeGroup(g1);

	track.close();

	const frames = await drainFrames(consumer, 200);
	// First 2 frames of group 0 survive; group 1 follows.
	expect(frames.map((f) => f.group)).toEqual([0, 0, 1]);
	expect(frames.map((f) => f.timestamp as number)).toEqual([1, 2, 4]);
	consumer.close();
});

test("Consumer close returns undefined from next()", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	const promise = consumer.next();
	consumer.close();

	const result = await promise;
	expect(result).toBeUndefined();
});

test("Consumer throws on concurrent next() calls", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	// First call blocks waiting for data
	void consumer.next();

	// Second call should throw
	expect(() => consumer.next()).toThrow("multiple calls to next not supported");
	consumer.close();
});

test("Consumer skips groups via PTS-span when over the max age", async () => {
	const track = new Track.Producer("test");
	// Zero max age = skip everything that's not the latest
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 0 as Time.Milli });

	// Write groups with increasing timestamps. With a 0 max age, any PTS span > 0 triggers skip.
	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [100_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Time.Micro]);
	track.close();

	const frames = await drainFrames(consumer, 300);
	// With a zero max age, the consumer should skip to the latest group
	const groups = [...new Set(frames.map((f) => f.group))];
	expect(groups.at(-1)).toBe(2);
	consumer.close();
});

// --- Ordering ---

test("Consumer delivers groups in sequence order regardless of arrival order", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	writeGroupWithLegacyFrames(track, 2, [60_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [30_000 as Time.Micro]);
	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await drainFrames(consumer, 500);
	expect(frames).toHaveLength(3);
	expect(frames[0].group).toBe(0);
	expect(frames[1].group).toBe(1);
	expect(frames[2].group).toBe(2);
	consumer.close();
});

test("Consumer delivers a group that arrives below the cursor", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	// Group.Producer 5 arrives first (sets active = 5)
	writeGroupWithLegacyFrames(track, 5, [100_000 as Time.Micro]);
	await new Promise((resolve) => setTimeout(resolve, 50));

	// Group.Producer 3 lands behind it, and Group.Producer 6 ahead of it. Arriving below the
	// cursor is not what makes content stale: how far behind the live edge a group may be is
	// the subscription's own max age, which `replay` deliberately opens wide here. So all
	// three are handed over, in sequence order, since delivery has not passed any of them yet.
	writeGroupWithLegacyFrames(track, 3, [0 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 6, [200_000 as Time.Micro]);
	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await drainFrames(consumer, 500);
	expect(frames.map((frame) => frame.group)).toEqual([3, 5, 6]);
	consumer.close();
});

// --- Group.Producer boundary signals ---

test("Consumer next() returns group-done signals", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Time.Micro]);
	track.close();

	await new Promise((resolve) => setTimeout(resolve, 50));

	const allResults: { frame: boolean; group: number }[] = [];
	for (;;) {
		const result = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), 500)),
		]);
		if (result === null || result === undefined) break;
		allResults.push({ frame: result.frame !== undefined, group: result.group });
	}

	const frameResults = allResults.filter((r) => r.frame);
	const boundaries = allResults.filter((r) => !r.frame);
	expect(frameResults).toHaveLength(3);
	expect(boundaries).toHaveLength(2);
	expect(boundaries[0].group).toBe(0);
	expect(boundaries[1].group).toBe(1);
	consumer.close();
});

test("Consumer reports a duration marker as metadata", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format: new LegacyFormat("video"), maxAge: 500 as Time.Milli });

	const group = new Group.Producer(0);
	group.writeFrame({
		payload: encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0xde, 0xad])),
		timestamp: Time.Timestamp.now(),
	});
	group.writeFrame({
		payload: encodeLegacyFrame(33_000 as Time.Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	group.close();
	track.writeGroup(group);
	track.close();
	await settle();

	const media = await consumer.next();
	expect(media?.frame?.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media?.frame?.keyframe).toBe(true);
	expect(media?.frame?.duration).toBeUndefined();

	const done = await consumer.next();
	expect(done?.frame).toBeUndefined();
	expect(done?.end).toBe(33_000 as Time.Micro);
	consumer.close();
});

test("Consumer skips a leading marker and keeps the first media keyframe", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format: new LegacyFormat("video"), maxAge: 500 as Time.Milli });

	const group = new Group.Producer(0);
	group.writeFrame({
		payload: encodeLegacyFrame(20_000 as Time.Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	group.writeFrame({
		payload: encodeLegacyFrame(20_000 as Time.Micro, new Uint8Array([0xde, 0xad])),
		timestamp: Time.Timestamp.now(),
	});
	group.close();
	track.writeGroup(group);
	track.close();

	const marker = await consumer.next();
	expect(marker?.end).toBe(20_000 as Time.Micro);
	expect(marker?.frame).toBeUndefined();
	const media = await consumer.next();
	expect(media?.frame?.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media?.frame?.keyframe).toBe(true);
	expect(media?.frame?.duration).toBeUndefined();
	consumer.close();
});

test("Consumer skips an empty LOC payload", async () => {
	const track = new Track.Producer("test");
	const producer = new LocProducer(track);
	producer.encode(new Uint8Array([0xde, 0xad]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array(), 33_000 as Time.Micro, false);
	producer.close();

	const consumer = new Consumer(replay(track), { format: new LocFormat("video"), maxAge: 500 as Time.Milli });
	await settle();
	const media = await consumer.next();
	expect(media?.frame?.payload).toEqual(new Uint8Array([0xde, 0xad]));
	expect(media?.frame?.duration).toBeUndefined();
	const marker = await consumer.next();
	expect(marker?.end).toBe(33_000 as Time.Micro);
	expect(marker?.frame).toBeUndefined();
	consumer.close();
});

test("Consumer preserves empty Legacy and LOC data frames", async () => {
	for (const kind of ["legacy", "loc"]) {
		const track = new Track.Producer("data");
		const producer =
			kind === "legacy" ? new LegacyProducer(track, new LegacyFormat("data")) : new LocProducer(track);
		producer.encode(new Uint8Array(), 0 as Time.Micro, true);
		producer.close();
		const format = kind === "legacy" ? new LegacyFormat("data") : new LocFormat();
		const consumer = new Consumer(replay(track), { format, maxAge: 500 as Time.Milli });
		const result = await consumer.next();
		expect(result?.frame?.payload).toEqual(new Uint8Array());
		expect(result?.frame?.keyframe).toBe(true);
		expect(result?.end).toBeUndefined();
		consumer.close();
	}
});

// --- Malformed rewind ---

/** Read until the next media frame, skipping the group-done markers in between. */
async function nextFrame(consumer: Consumer) {
	for (;;) {
		const result = await consumer.next();
		if (!result || result.frame) return result;
	}
}

test("Consumer aborts a group below the live edge", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 30_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [10_000_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [0 as Time.Micro, 100_000 as Time.Micro]);
	await settle();

	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(10_000_000 as Time.Micro);
	await expect(nextFrame(consumer)).rejects.toThrow("below the live edge");

	consumer.close();
});

test("Consumer aborts a rewind carried by a later arrival", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 30_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [10_000_000 as Time.Micro]);
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(10_000_000 as Time.Micro);
	expect((await consumer.next())?.frame).toBeUndefined();

	const pending = nextFrame(consumer);
	await settle();
	writeGroupWithLegacyFrames(track, 1, [0 as Time.Micro, 100_000 as Time.Micro]);

	await expect(pending).rejects.toThrow("below the live edge");

	consumer.close();
});

// Decode order dips below presentation order inside every group with B-frames. That is not a
// rewind, so the live edge the detector compares against has to be the group's own.
test("Consumer treats B-frame reordering within a group as continuous", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 30_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 66_000 as Time.Micro, 33_000 as Time.Micro]);
	await settle();
	for (const timestamp of [0, 66_000, 33_000]) {
		const result = await nextFrame(consumer);
		expect(result?.frame?.timestamp).toBe(timestamp as Time.Micro);
		expect(result?.discontinuity).toBe(0);
	}

	// The cursor is now on group 1, whose own frames reorder the same way.
	writeGroupWithLegacyFrames(track, 1, [100_000 as Time.Micro, 166_000 as Time.Micro, 133_000 as Time.Micro]);
	await settle();
	for (const timestamp of [100_000, 166_000, 133_000]) {
		const result = await nextFrame(consumer);
		expect(result?.frame?.timestamp).toBe(timestamp as Time.Micro);
		expect(result?.discontinuity).toBe(0);
		expect(result?.continuous).toBe(true);
	}

	consumer.close();
});

test("Consumer accepts open-GOP leading pictures above the previous group", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 30_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Time.Micro, 50_000 as Time.Micro]);
	await settle();
	for (const timestamp of [0, 33_000, 66_000, 50_000]) {
		const result = await nextFrame(consumer);
		expect(result?.frame?.timestamp).toBe(timestamp as Time.Micro);
		expect(result?.discontinuity).toBe(0);
	}

	consumer.close();
});

// --- Buffered signal ---

test("Consumer buffered signal updates as frames arrive", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 500 as Time.Milli });

	expect(consumer.buffered.peek()).toEqual([]);

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [66_000 as Time.Micro, 99_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 100));

	const ranges = consumer.buffered.peek();
	expect(ranges.length).toBe(1);
	expect(ranges[0].start).toBe(0 as Time.Milli);
	expect((ranges[0].end as number) >= 66).toBeTruthy();

	track.close();
	consumer.close();
});

// --- Gap recovery ---

test("Consumer recovers from gap in group sequence numbers", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 100 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 20_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [40_000 as Time.Micro, 60_000 as Time.Micro]);
	// Skip group 2
	writeGroupWithLegacyFrames(track, 3, [120_000 as Time.Micro, 140_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 4, [160_000 as Time.Micro, 180_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 5, [200_000 as Time.Micro, 220_000 as Time.Micro]);
	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

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
			return [{ payload: new Uint8Array([1]), timestamp: 33_000 as Time.Micro, keyframe: false }];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: emptyThenValid, maxAge: 500 as Time.Milli });

	const group = new Group.Producer(0);
	group.writeFrame({ payload: new Uint8Array([0x01]), timestamp: Time.Timestamp.now() }); // empty decode
	group.writeFrame({ payload: new Uint8Array([0x02]), timestamp: Time.Timestamp.now() }); // valid decode
	group.close();
	track.writeGroup(group);
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
			return [{ payload: new Uint8Array(), timestamp: 0 as Time.Micro, keyframe: false }];
		},
	};
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format, maxAge: 500 as Time.Milli });
	const group = new Group.Producer(0);
	group.writeFrame({ payload: new Uint8Array([1]), timestamp: Time.Timestamp.now() });
	group.close();
	track.writeGroup(group);
	track.close();

	const result = await consumer.next();
	expect(result?.frame?.payload).toHaveLength(0);
	consumer.close();
});

// --- CMAF through Consumer ---

test("Consumer with CmafFormat delivers correct timestamps", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 500 as Time.Milli,
	});

	const group = new Group.Producer(0);
	group.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0xca, 0xfe]),
			timestamp: 0,
			duration: 3000,
			keyframe: true,
			sequence: 0,
		}),
		timestamp: Time.Timestamp.now(),
	});
	group.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0xbe, 0xef]),
			timestamp: 3000,
			duration: 3000,
			keyframe: false,
			sequence: 0,
		}),
		timestamp: Time.Timestamp.now(),
	});
	group.close();
	track.writeGroup(group);
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(2);
	expect(frames[0].keyframe).toBe(true); // index 0 override
	expect(frames[1].keyframe).toBe(false); // trusts format
	expect(frames[0].timestamp).toBe(0 as Time.Micro);
	expect(frames[1].timestamp).toBe(33_333 as Time.Micro); // 3000/90000 * 1_000_000
	consumer.close();
});

test("CmafFormat decodes the per-sample duration", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		kind: "video",
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const [frame] = format.decode(segment);
	// 3000 ticks / 90000 timescale * 1_000_000 = 33333µs
	expect(frame.duration).toBe(33_333 as Time.Micro);
});

// --- Duration skipping ---

// Format whose frames carry a fixed 33ms duration; the timestamp is byte 0 (ms).
const durationFormat: ContainerFormat = {
	decode(frame: Uint8Array): Frame[] {
		return [
			{
				payload: frame,
				timestamp: (frame[0] * 1000) as Time.Micro,
				duration: 33_000 as Time.Micro,
				keyframe: false,
			},
		];
	},
};

test("Consumer duration-skips a stalled group once it is covered", async () => {
	const track = new Track.Producer("test");
	// Latency dwarfs the gap, so only duration coverage can trigger the skip.
	const consumer = new Consumer(replay(track), { format: durationFormat, maxAge: 10_000 as Time.Milli });

	// Group.Producer 0: one frame at ts=0 lasting 33ms, never closed (stalled).
	const g0 = new Group.Producer(0);
	g0.writeFrame({ payload: new Uint8Array([0]), timestamp: Time.Timestamp.now() });

	// Group.Producer 1: closed, starts exactly where group 0's frame ends.
	const g1 = new Group.Producer(1);
	g1.writeFrame({ payload: new Uint8Array([33]), timestamp: Time.Timestamp.now() });
	g1.close();

	track.writeGroup(g0);
	track.writeGroup(g1);
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames.map((f) => f.timestamp as number)).toEqual([0, 33_000]);
	expect(frames.map((f) => f.group)).toEqual([0, 1]);
	consumer.close();
});

test("Consumer does not duration-skip when the gap is not covered", async () => {
	// Format whose frames last only 10ms, short of the 33ms gap to the next group.
	const shortFormat: ContainerFormat = {
		decode(frame: Uint8Array): Frame[] {
			return [
				{
					payload: frame,
					timestamp: (frame[0] * 1000) as Time.Micro,
					duration: 10_000 as Time.Micro,
					keyframe: false,
				},
			];
		},
	};

	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: shortFormat, maxAge: 10_000 as Time.Milli });

	// Group.Producer 0 stays open and later receives a second frame; nothing covers the gap,
	// so that late frame must survive rather than being skipped.
	const g0 = new Group.Producer(0);
	g0.writeFrame({ payload: new Uint8Array([0]), timestamp: Time.Timestamp.now() });

	const g1 = new Group.Producer(1);
	g1.writeFrame({ payload: new Uint8Array([33]), timestamp: Time.Timestamp.now() });
	g1.close();

	track.writeGroup(g0);
	track.writeGroup(g1);

	// Let the consumer settle on group 0, then extend it before closing.
	await new Promise((resolve) => setTimeout(resolve, 20));
	g0.writeFrame({ payload: new Uint8Array([20]), timestamp: Time.Timestamp.now() });
	g0.close();
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
test("Consumer delivers a PTS-contiguous next group whose sequence jumped (CMAF)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 500 as Time.Milli,
	});

	// Group A (seq 1_000_000): one 3000-tick sample, so its content ends at 33_333µs (3000/90000 * 1e6).
	const a = new Group.Producer(1_000_000);
	a.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0x01]),
			timestamp: 0,
			duration: 3000,
			keyframe: true,
			sequence: 0,
		}),
		timestamp: Time.Timestamp.now(),
	});
	a.close();
	track.writeGroup(a);

	const firstFrame = await consumer.next();
	expect(firstFrame?.frame?.payload).toEqual(new Uint8Array([0x01]));
	await consumer.next(); // group-done marker

	const pending = consumer.next();

	// Group B: sequence jumps +90_000, and its first PTS (3045 ticks ~= 33_833µs) sits ~500µs past
	// A.end (33_333µs) -- the sub-millisecond skew a genuinely contiguous CMAF boundary shows because
	// per-sample durations and base-decode-times round to µs independently. It's within the contiguity
	// tolerance, so the timeline is unbroken and B must deliver immediately even while still open.
	const b = new Group.Producer(1_090_000);
	track.writeGroup(b);
	b.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0x02]),
			timestamp: 3045,
			duration: 3000,
			keyframe: true,
			sequence: 1,
		}),
		timestamp: Time.Timestamp.now(),
	});

	const result = await Promise.race([
		pending,
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 500)),
	]);
	expect(result).not.toBe("timeout");
	expect((result as { frame?: Frame } | undefined)?.frame?.payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// A real PTS gap (the active group's end does NOT reach the next buffered group's first frame) means
// an intermediate group may still be in transit, so we must NOT skip ahead and wreck ordering.
// Delivery resumes only once the missing, timeline-continuous group arrives.
test("Consumer waits on a PTS gap instead of skipping to a later buffered group (CMAF)", async () => {
	const track = new Track.Producer("test");
	// Large max age so the gap can't be age-skipped during the test window.
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 10_000 as Time.Milli,
	});

	// Group A (seq 1_000_000): content ends at 33_333µs.
	const a = new Group.Producer(1_000_000);
	a.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0x01]),
			timestamp: 0,
			duration: 3000,
			keyframe: true,
			sequence: 0,
		}),
		timestamp: Time.Timestamp.now(),
	});
	a.close();
	track.writeGroup(a);

	const firstFrame = await consumer.next();
	expect(firstFrame?.frame?.payload).toEqual(new Uint8Array([0x01]));
	await consumer.next(); // group-done marker

	const pending = consumer.next();

	// Group C (seq 1_090_000) starts at 90_000 ticks (1_000_000µs) -- far past A.end (33_333µs), a real
	// gap. An intermediate group may still be in transit, so C must NOT be delivered yet.
	const c = new Group.Producer(1_090_000);
	track.writeGroup(c);
	c.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0x03]),
			timestamp: 90_000,
			duration: 3000,
			keyframe: true,
			sequence: 1,
		}),
		timestamp: Time.Timestamp.now(),
	});

	const gap = await Promise.race([
		pending,
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 300)),
	]);
	expect(gap).toBe("timeout"); // held: the gap is not eagerly skipped

	// The missing group B arrives, contiguous with A (first PTS 3000 ticks = 33_333µs = A.end); it
	// continues the timeline, so delivery resumes with B's frame.
	const b = new Group.Producer(1_045_000);
	track.writeGroup(b);
	b.writeFrame({
		payload: encodeDataSegment({
			kind: "video",
			data: new Uint8Array([0x02]),
			timestamp: 3000,
			duration: 3000,
			keyframe: true,
			sequence: 2,
		}),
		timestamp: Time.Timestamp.now(),
	});

	const result = await pending;
	expect((result as { frame?: Frame } | undefined)?.frame?.payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// A group can finish before it becomes the active one (it arrived and completed while an earlier
// group was still open). The cursor then moves past it in next() rather than in #runGroup, so that
// is where its presentation end has to be recorded. Miss it and the stale end from the group before
// blocks every later contiguous group.
test("Consumer delivers a contiguous group after one that completed out of order (CMAF)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 10_000 as Time.Milli,
	});

	// A (seq 1_000_000) stays open so it remains the active group.
	const a = new Group.Producer(1_000_000);
	track.writeGroup(a);
	a.writeFrame({ payload: encodeCmafFrame(0x01, 0, 0), timestamp: Time.Timestamp.now() });

	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x01]));

	// A's second frame stays buffered so A can't be duration-skipped. A ends at 6000 ticks (66_666µs).
	a.writeFrame({ payload: encodeCmafFrame(0x02, 3000, 1), timestamp: Time.Timestamp.now() });
	await settle();

	// B (seq 1_045_000) starts at A's end and is CLOSED while A is still active, so B's own finally
	// block sees sequence !== #active and never records anything. B ends at 9000 ticks (100_000µs).
	const b = new Group.Producer(1_045_000);
	track.writeGroup(b);
	b.writeFrame({ payload: encodeCmafFrame(0x03, 6000, 2), timestamp: Time.Timestamp.now() });
	b.close();
	await settle();

	a.close();
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x02]));
	expect((await consumer.next())?.frame).toBeUndefined(); // A group-done
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x03]));
	expect((await consumer.next())?.frame).toBeUndefined(); // B group-done

	// C (seq 1_090_000) starts at B's end, so it continues the timeline and must deliver.
	const pending = consumer.next();
	const c = new Group.Producer(1_090_000);
	track.writeGroup(c);
	c.writeFrame({ payload: encodeCmafFrame(0x04, 9000, 3), timestamp: Time.Timestamp.now() });

	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).not.toBe("timeout");
	expect((result as { frame?: Frame } | undefined)?.frame?.payload).toEqual(new Uint8Array([0x04]));

	consumer.close();
});

// A resubscribe can replay a stale group, lose the group after it, then carry on live. Once the live
// head reaches past where presentation left off by more than the budget, whatever is missing would
// arrive too old to play, so the head is the group to play next and must not be dropped as "slow".
test("Consumer plays the head once a waited-out gap exceeds the budget (CMAF)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 100 as Time.Milli,
	});

	// A (seq 1000): one frame, ends at 3000 ticks (33_333µs).
	const a = new Group.Producer(1000);
	track.writeGroup(a);
	a.writeFrame({ payload: encodeCmafFrame(0x01, 0, 0), timestamp: Time.Timestamp.now() });
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x01]));
	a.close();
	expect((await consumer.next())?.frame).toBeUndefined(); // #active falls back to the 1001 phantom

	// The reader is parked on the missing 1001, as a decoder is while it waits for the next group.
	const pending = consumer.next();

	// B (seq 2000) starts at 90_000 ticks (1_000_000µs), a second past A's end: the missing group
	// could only ever arrive far beyond the 100ms budget, so B plays from its first frame.
	const b = new Group.Producer(2000);
	track.writeGroup(b);
	b.writeFrame({ payload: encodeCmafFrame(0x02, 90_000, 1), timestamp: Time.Timestamp.now() });

	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).not.toBe("timeout");
	const delivered = result as { frame?: Frame; continuous?: boolean } | undefined;
	expect(delivered?.frame?.payload).toEqual(new Uint8Array([0x02]));
	// The missing group's span was never delivered, so downstream must not bridge it.
	expect(delivered?.continuous).toBe(false);

	consumer.close();
});

// A gap still inside the budget is waited out: the missing group may yet arrive in time.
test("Consumer waits on a gap within the budget, then plays the head once it exceeds it (CMAF)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 100 as Time.Milli,
	});

	// A (seq 1000): one frame, ends at 3000 ticks (33_333µs).
	const a = new Group.Producer(1000);
	track.writeGroup(a);
	a.writeFrame({ payload: encodeCmafFrame(0x01, 0, 0), timestamp: Time.Timestamp.now() });
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x01]));
	a.close();
	expect((await consumer.next())?.frame).toBeUndefined();

	// B (seq 2000) starts at 6000 ticks (66_667µs): a gap, but only 33ms past A's end.
	const b = new Group.Producer(2000);
	track.writeGroup(b);
	b.writeFrame({ payload: encodeCmafFrame(0x02, 6000, 1), timestamp: Time.Timestamp.now() });

	const pending = consumer.next();
	const early = await Promise.race([pending, settle(50).then(() => "waiting" as const)]);
	expect(early).toBe("waiting");

	// B alone grows past the budget (up to 12_000 ticks, 100ms past A's end and beyond).
	for (let i = 1; i <= 3; i++) {
		b.writeFrame({ payload: encodeCmafFrame(0x02, 6000 + i * 3000, 1 + i), timestamp: Time.Timestamp.now() });
		await settle(10);
	}

	const result = await Promise.race([pending, settle(300).then(() => "timeout" as const)]);
	expect(result).not.toBe("timeout");
	expect((result as { frame?: Frame } | undefined)?.frame?.payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});

// --- Continuity reporting ---

// `continuous` is what downstream buffer accounting keys on, so it must be false exactly when the
// consumer dropped something. Group numbers can't answer that: they aren't required to be
// sequential, and adjacency doesn't rule out a group the max age check dropped on the way past.
test("Consumer reports continuity while nothing is dropped", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 100 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	await settle();

	expect((await consumer.next())?.continuous).toBe(false); // nothing precedes the first frame
	expect((await consumer.next())?.continuous).toBe(true); // same group: consecutive by protocol
	expect((await consumer.next())?.frame).toBeUndefined(); // group 0 done

	writeGroupWithLegacyFrames(track, 1, [66_000 as Time.Micro]);
	await settle();
	expect((await consumer.next())?.continuous).toBe(true); // group 1 continues group 0

	consumer.close();
});

test("Consumer reports a marker group as a playhead event", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxAge: 2_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	writeMarkerGroup(track, 1, 0 as Time.Micro);
	writeGroupWithLegacyFrames(track, 2, [1_000_000 as Time.Micro]);
	await settle();

	expect((await consumer.next())?.discontinuity).toBe(0);
	expect((await consumer.next())?.discontinuity).toBe(0); // group 0 done
	const reset = await consumer.next();
	expect(reset?.frame).toBeUndefined();
	expect(reset?.discontinuity).toBe(1);
	expect(reset?.continuous).toBe(false);
	expect((await nextFrame(consumer))?.discontinuity).toBe(1);

	consumer.close();
});

test("Consumer empty groups mean nothing", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxAge: 2_000 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	const empty = new Group.Producer(1);
	track.writeGroup(empty);
	empty.close();
	writeGroupWithLegacyFrames(track, 2, [1_000 as Time.Micro]);
	await settle();

	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Time.Micro);
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(1_000 as Time.Micro);
	expect(consumer.discontinuity).toBe(0);

	consumer.close();
});

test("Consumer latency skip bumps playhead generation once", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxAge: 0 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Time.Micro);
	expect((await consumer.next())?.frame).toBeUndefined();

	writeGroupWithLegacyFrames(track, 2, [100_000 as Time.Micro]);
	await settle();

	const resumed = await nextFrame(consumer);
	expect(resumed?.frame?.timestamp).toBe(100_000 as Time.Micro);
	expect(resumed?.discontinuity).toBe(1);

	consumer.close();
});

test("Consumer jumps the playhead after a shed marker with a timestamp hole", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxAge: 0 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	await settle();
	expect((await consumer.next())?.frame?.timestamp).toBe(0 as Time.Micro);
	expect((await consumer.next())?.frame).toBeUndefined();

	writeGroupWithLegacyFrames(track, 3, [1_000_000 as Time.Micro]);
	await settle();

	const resumed = await nextFrame(consumer);
	expect(resumed?.frame?.timestamp).toBe(1_000_000 as Time.Micro);
	expect(resumed?.discontinuity).toBe(1);

	consumer.close();
});

test("Consumer zero-budget skip keeps a contiguous marker", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), { format: new LegacyFormat("audio"), maxAge: 0 as Time.Milli });

	const group0 = new Group.Producer(0);
	track.writeGroup(group0);
	group0.writeFrame({
		payload: encodeLegacy(0 as Time.Micro),
		timestamp: Time.Timestamp.now(),
	});
	await settle();
	expect((await nextFrame(consumer))?.frame?.timestamp).toBe(0 as Time.Micro);

	writeMarkerGroup(track, 1, 0 as Time.Micro);
	writeGroupWithLegacyFrames(track, 2, [500 as Time.Micro]);
	await settle();

	const resumed = await nextFrame(consumer);
	expect(resumed?.frame?.timestamp).toBe(500 as Time.Micro);
	expect(resumed?.discontinuity).toBe(1);
	group0.close();

	consumer.close();
});

// The case the group-number heuristic got backwards: ids jump, but the PTS timeline is unbroken.
test("Consumer reports continuity across a PTS-contiguous group id jump (CMAF)", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(replay(track), {
		format: new CmafFormat(TEST_INIT),
		maxAge: 10_000 as Time.Milli,
	});

	const a = new Group.Producer(1_000_000);
	track.writeGroup(a);
	a.writeFrame({ payload: encodeCmafFrame(0x01, 0, 0), timestamp: Time.Timestamp.now() });
	a.close();

	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([0x01]));
	expect((await consumer.next())?.frame).toBeUndefined(); // group-done

	// Sequence jumps +90_000 but the first PTS meets A's end, so nothing is missing.
	const b = new Group.Producer(1_090_000);
	track.writeGroup(b);
	b.writeFrame({ payload: encodeCmafFrame(0x02, 3000, 1), timestamp: Time.Timestamp.now() });

	const result = await consumer.next();
	expect(result?.frame?.payload).toEqual(new Uint8Array([0x02]));
	expect(result?.continuous).toBe(true);
	expect(result?.discontinuity).toBe(0);

	consumer.close();
});

test("live duration marker follows an immediately delivered video frame", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format: new LegacyFormat("video"), maxAge: 500 as Time.Milli });
	const group = track.appendGroup();
	group.writeFrame({
		payload: encodeLegacyFrame(0 as Time.Micro, new Uint8Array([1])),
		timestamp: Time.Timestamp.now(),
	});
	let delivered = false;
	const read = consumer.next().then((value) => {
		delivered = true;
		return value;
	});
	await settle();
	expect(delivered).toBe(true);
	expect((await read)?.frame?.timestamp).toBe(0 as Time.Micro);
	group.writeFrame({
		payload: encodeLegacyFrame(15_000 as Time.Micro, new Uint8Array()),
		timestamp: Time.Timestamp.now(),
	});
	const result = await consumer.next();
	expect(result?.frame).toBeUndefined();
	expect(result?.end).toBe(15_000 as Time.Micro);
	consumer.close();
	track.close();
});

test("audio cut writes no duration marker", async () => {
	const track = new Track.Producer("test");
	const subscriber = track.subscribe();
	const producer = new LegacyProducer(track, new LegacyFormat("audio"));
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.cut(15_000 as Time.Micro);
	const group = await subscriber.recvGroup();
	expect(await group?.readFrame()).toBeDefined();
	expect(await group?.readFrame()).toBeUndefined();
	producer.close();
});

for (const code of [StreamCode.Cancel, StreamCode.Internal, StreamCode.Old, StreamCode.Evicted, StreamCode(1234)]) {
	test(`Consumer preserves frames and continues after group reset ${code}`, async () => {
		const track = new Track.Producer("test");
		const consumer = new Consumer(replay(track), { format: new LegacyFormat("data"), maxAge: 0 as Time.Milli });
		const error = spyOn(console, "error").mockImplementation(() => {});
		try {
			const group = track.appendGroup();
			group.writeFrame({
				payload: encodeLegacyFrame(0 as Time.Micro, new Uint8Array([1])),
				timestamp: Time.Timestamp.now(),
			});
			await settle();
			group.close(new NetError.Stream(code));
			await settle();
			expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([1]));
			expect((await consumer.next())?.frame).toBeUndefined();
			const next = track.appendGroup();
			next.writeFrame({
				payload: encodeLegacyFrame(1 as Time.Micro, new Uint8Array([2])),
				timestamp: Time.Timestamp.now(),
			});
			next.close();
			const resumed = await consumer.next();
			expect(resumed?.frame?.payload).toEqual(new Uint8Array([2]));
			expect(resumed?.continuous).toBe(false);
			expect(resumed?.frame?.keyframe).toBe(true);
			expect(error).not.toHaveBeenCalled();
		} finally {
			consumer.close();
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
			track.close(end ?? undefined);
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
	const error = spyOn(console, "error").mockImplementation(() => {});
	try {
		const reset = track.appendGroup();
		await settle();
		reset.close(new NetError.Stream(StreamCode.Cancel));
		await settle();
		const malformed = track.appendGroup();
		malformed.writeFrame({ payload: new Uint8Array(), timestamp: Time.Timestamp.now() });
		malformed.close();
		await settle();
		expect(error).toHaveBeenCalledWith("spawn error", expect.any(Error));
	} finally {
		consumer.close();
		track.close();
		error.mockRestore();
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
			maxAge: 10_000 as Time.Milli,
		});
		try {
			writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
			expect((await consumer.next())?.group).toBe(0);
			expect((await consumer.next())?.frame).toBeUndefined();
			writeGroupWithLegacyFrames(track, 2, [1_000_000 as Time.Micro]);
			await settle();
			const pending = consumer.next();
			track.close(end ?? undefined);
			const resumed = await Promise.race([pending, settle(100).then(() => "stalled" as const)]);
			expect(resumed).not.toBe("stalled");
			if (resumed === "stalled") return;
			expect(resumed?.group).toBe(2);
			expect(resumed?.frame?.timestamp).toBe(1_000_000 as Time.Micro);
			expect(resumed?.continuous).toBe(false);
			expect((await consumer.next())?.frame).toBeUndefined();
			if (end) await expect(consumer.next()).rejects.toBe(end);
			else expect(await consumer.next()).toBeUndefined();
		} finally {
			consumer.close();
			track.close();
		}
	});
}
