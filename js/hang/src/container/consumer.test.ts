import { expect, test } from "bun:test";
import { Group, type Time, Track, Varint } from "@moq/net";
import type { InitSegment } from "./cmaf/decode.ts";
import { encodeDataSegment } from "./cmaf/encode.ts";
import { Format as CmafFormat } from "./cmaf/format.ts";
import { Consumer } from "./consumer.ts";
import type { Format as ContainerFormat } from "./format.ts";
import { Format as LegacyFormat } from "./legacy.ts";
import type { Frame } from "./types.ts";

const TIMESCALE = 90_000;
const TEST_INIT: InitSegment = {
	timescale: TIMESCALE,
	trackId: 1,
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

// --- LegacyFormat ---

test("LegacyFormat decodes a valid frame", () => {
	const format = new LegacyFormat();
	const payload = new Uint8Array([0xde, 0xad]);
	const timestamp = 1000 as Time.Micro;
	const frame = encodeLegacyFrame(timestamp, payload);

	const result = format.decode(frame);

	expect(result).toHaveLength(1);
	expect(result[0].timestamp).toBe(timestamp);
	expect(result[0].data).toEqual(payload);
	expect(result[0].keyframe).toBe(false);
});

test("LegacyFormat always returns keyframe: false", () => {
	const format = new LegacyFormat();
	const frame = encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0x01]));

	const [decoded] = format.decode(frame);
	expect(decoded.keyframe).toBe(false);
});

test("LegacyFormat always returns exactly one frame", () => {
	const format = new LegacyFormat();
	const frame = encodeLegacyFrame(5000 as Time.Micro, new Uint8Array([0x01, 0x02, 0x03]));

	const result = format.decode(frame);
	expect(result).toHaveLength(1);
});

test("LegacyFormat throws on empty input", () => {
	const format = new LegacyFormat();
	expect(() => format.decode(new Uint8Array(0))).toThrow();
});

test("LegacyFormat throws on truncated input", () => {
	const format = new LegacyFormat();
	// A varint that indicates more bytes follow but is truncated
	expect(() => format.decode(new Uint8Array([0x80]))).toThrow();
});

// --- CmafFormat ---

test("CmafFormat decodes a valid keyframe segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
		data: new Uint8Array([0xca, 0xfe]),
		timestamp: 0,
		duration: 3000,
		keyframe: true,
		sequence: 0,
	});

	const result = format.decode(segment);

	expect(result).toHaveLength(1);
	expect(result[0].data).toEqual(new Uint8Array([0xca, 0xfe]));
	expect(result[0].timestamp).toBe(0 as Time.Micro);
	expect(result[0].keyframe).toBe(true);
});

test("CmafFormat decodes a delta frame segment", () => {
	const format = new CmafFormat(TEST_INIT);
	const segment = encodeDataSegment({
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

function writeGroupWithLegacyFrames(track: Track, sequence: number, timestamps: Time.Micro[]) {
	const group = new Group(sequence);
	for (const ts of timestamps) {
		group.writeFrame(encodeLegacy(ts));
	}
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
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro, 33_000 as Time.Micro]);
	track.close();

	const frames = await drainFrames(consumer, 200);
	expect(frames).toHaveLength(2);
	expect(frames[0].timestamp).toBe(0 as Time.Micro);
	expect(frames[1].timestamp).toBe(33_000 as Time.Micro);
	consumer.close();
});

test("Consumer forces keyframe true at index 0", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

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
				{ data: new Uint8Array([1]), timestamp: 0 as Time.Micro, keyframe: false },
				{ data: new Uint8Array([2]), timestamp: 33_000 as Time.Micro, keyframe: false },
				{ data: new Uint8Array([3]), timestamp: 66_000 as Time.Micro, keyframe: false },
			];
		},
	};

	const track = new Track("test");
	const consumer = new Consumer(track, { format: multiFormat, latency: 500 as Time.Milli });

	const group = new Group(0);
	group.writeFrame(new Uint8Array([0x01])); // first MoQ frame → 3 samples
	group.writeFrame(new Uint8Array([0x02])); // second MoQ frame → 3 samples
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
			return [{ data: frame, timestamp: frame[0] as Time.Micro, keyframe: false }];
		},
	};

	const track = new Track("test");
	const consumer = new Consumer(track, { format: truncatingFormat, latency: 500 as Time.Milli });

	// Group 0: 2 valid frames then a tail-truncating error.
	const g0 = new Group(0);
	g0.writeFrame(new Uint8Array([0x01]));
	g0.writeFrame(new Uint8Array([0x02]));
	g0.writeFrame(new Uint8Array([0xff]));
	g0.close();
	track.writeGroup(g0);

	// Group 1 decodes cleanly.
	const g1 = new Group(1);
	g1.writeFrame(new Uint8Array([0x04]));
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
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

	const promise = consumer.next();
	consumer.close();

	const result = await promise;
	expect(result).toBeUndefined();
});

test("Consumer throws on concurrent next() calls", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

	// First call blocks waiting for data
	consumer.next();

	// Second call should throw
	expect(() => consumer.next()).toThrow("multiple calls to next not supported");
	consumer.close();
});

test("Consumer skips groups via PTS-span when over latency", async () => {
	const track = new Track("test");
	// Zero latency = skip everything that's not the latest
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 0 as Time.Milli });

	// Write groups with increasing timestamps. With 0 latency, any PTS span > 0 triggers skip.
	writeGroupWithLegacyFrames(track, 0, [0 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 1, [100_000 as Time.Micro]);
	writeGroupWithLegacyFrames(track, 2, [200_000 as Time.Micro]);
	track.close();

	const frames = await drainFrames(consumer, 300);
	// With zero latency, the consumer should skip to the latest group
	const groups = [...new Set(frames.map((f) => f.group))];
	expect(groups.at(-1)).toBe(2);
	consumer.close();
});

// --- Ordering ---

test("Consumer delivers groups in sequence order regardless of arrival order", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

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

test("Consumer rejects stale groups", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

	// Group 5 arrives first (sets active = 5)
	writeGroupWithLegacyFrames(track, 5, [0 as Time.Micro]);
	await new Promise((resolve) => setTimeout(resolve, 50));

	// Group 3 is stale
	writeGroupWithLegacyFrames(track, 3, [100_000 as Time.Micro]);
	// Group 6 is valid
	writeGroupWithLegacyFrames(track, 6, [30_000 as Time.Micro]);
	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await drainFrames(consumer, 500);
	expect(frames).toHaveLength(2);
	expect(frames[0].group).toBe(5);
	expect(frames[1].group).toBe(6);
	consumer.close();
});

// --- Group boundary signals ---

test("Consumer next() returns group-done signals", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

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

// --- Buffered signal ---

test("Consumer buffered signal updates as frames arrive", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 500 as Time.Milli });

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
	const track = new Track("test");
	const consumer = new Consumer(track, { format: new LegacyFormat(), latency: 100 as Time.Milli });

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
			return [{ data: new Uint8Array([1]), timestamp: 33_000 as Time.Micro, keyframe: false }];
		},
	};

	const track = new Track("test");
	const consumer = new Consumer(track, { format: emptyThenValid, latency: 500 as Time.Milli });

	const group = new Group(0);
	group.writeFrame(new Uint8Array([0x01])); // empty decode
	group.writeFrame(new Uint8Array([0x02])); // valid decode
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

// --- CMAF through Consumer ---

test("Consumer with CmafFormat delivers correct timestamps", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, {
		format: new CmafFormat(TEST_INIT),
		latency: 500 as Time.Milli,
	});

	const group = new Group(0);
	group.writeFrame(
		encodeDataSegment({
			data: new Uint8Array([0xca, 0xfe]),
			timestamp: 0,
			duration: 3000,
			keyframe: true,
			sequence: 0,
		}),
	);
	group.writeFrame(
		encodeDataSegment({
			data: new Uint8Array([0xbe, 0xef]),
			timestamp: 3000,
			duration: 3000,
			keyframe: false,
			sequence: 0,
		}),
	);
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
