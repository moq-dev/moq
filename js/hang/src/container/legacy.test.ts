import assert from "node:assert";
import test from "node:test";
import { Time, Track } from "@moq/lite";
import { Consumer, Producer } from "./legacy.ts";

// Helper: encode a frame using the legacy container format (varint timestamp + payload).
function encodeFrame(producer: Producer, timestamp: Time.Micro, keyframe: boolean) {
	producer.encode(new Uint8Array([0xde, 0xad]), timestamp, keyframe);
}

// Helper: write a group with multiple frames to a track.
function writeGroup(
	producer: Producer,
	groupIndex: number,
	framesPerGroup: number,
	frameSpacing: Time.Micro,
	groupSpacing: Time.Micro,
) {
	for (let f = 0; f < framesPerGroup; f++) {
		const timestamp = Time.Micro.add(Time.Micro.mul(groupSpacing, groupIndex), Time.Micro.mul(frameSpacing, f));
		encodeFrame(producer, timestamp, f === 0);
	}
}

// Drain all available frames from the consumer with a timeout.
async function consumeFrames(consumer: Consumer, timeout: number): Promise<{ timestamp: Time.Micro; group: number }[]> {
	const frames: { timestamp: Time.Micro; group: number }[] = [];

	for (;;) {
		const result = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), timeout)),
		]);

		if (result === null) break; // timeout
		if (result === undefined) break; // closed
		if (result.frame) {
			frames.push({ timestamp: result.frame.timestamp, group: result.group });
		}
	}

	return frames;
}

test("consumer reads frames from a single group", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	encodeFrame(producer, 0 as Time.Micro, true);
	producer.close();

	const frames = await consumeFrames(consumer, 200);
	assert.strictEqual(frames.length, 1);
	assert.strictEqual(frames[0].timestamp, 0);

	consumer.close();
});

test("consumer reads frames from multiple groups within latency", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// 5 groups with 1 frame each, 20ms apart. Total span = 80ms, well within 500ms.
	for (let i = 0; i < 5; i++) {
		encodeFrame(producer, (i * 20_000) as Time.Micro, true);
	}
	producer.close();

	const frames = await consumeFrames(consumer, 200);
	assert.strictEqual(frames.length, 5, `Expected 5 frames, got ${frames.length}`);

	consumer.close();
});

test("active index advances correctly after latency skip", async () => {
	const track = new Track("test");
	const producer = new Producer(track);

	// Latency target: 100ms.
	const consumer = new Consumer(track, { latency: 100 as Time.Milli });

	// Write 20 groups, each with 5 frames.
	// Group spacing: 15ms. Frame spacing: 2ms.
	// Total span: 19*15+4*2 = 293ms, well over 100ms.
	//
	// The bug: when #checkLatency skips groups and sets #active to a new group,
	// that group's #runGroup may have already finished (its finally block ran
	// when #active was still pointing at an earlier group). Since the finally
	// block only advances #active when group.sequence === #active, #active
	// becomes permanently stuck. The consumer reads frames from the stuck
	// group and then deadlocks waiting for #notify that never comes.
	const groupCount = 20;
	const framesPerGroup = 5;
	const groupSpacing = 15_000 as Time.Micro;
	const frameSpacing = 2_000 as Time.Micro;

	for (let g = 0; g < groupCount; g++) {
		writeGroup(producer, g, framesPerGroup, frameSpacing, groupSpacing);
	}
	producer.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await consumeFrames(consumer, 200);

	// Expected: skip groups until remaining span < 100ms, then deliver the rest.
	//   Group 13 starts at 195ms. Span from 13 to 19: 293-195 = 98ms < 100ms.
	//   So groups 0-12 should be skipped, groups 13-19 survive = 35 frames.
	//
	// Bug: #active gets stuck at group 13 (whose #runGroup already completed),
	// so the consumer only gets 5 frames from group 13, then deadlocks.
	assert.ok(frames.length >= 30, `Expected >= 30 frames (groups 13-19), got ${frames.length}`);

	consumer.close();
});

// --- Consumer lifecycle ---

test("consumer.close() stops consumption", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write some groups
	for (let i = 0; i < 3; i++) {
		encodeFrame(producer, (i * 10_000) as Time.Micro, true);
	}

	// Let async processing start
	await new Promise((resolve) => setTimeout(resolve, 50));

	// Close the consumer mid-consumption
	consumer.close();

	// next() should return undefined after close
	const result = await consumer.next();
	assert.strictEqual(result, undefined);

	producer.close();
});

test("consumer.close() before any reads", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	for (let i = 0; i < 3; i++) {
		encodeFrame(producer, (i * 10_000) as Time.Micro, true);
	}

	// Close immediately without reading
	consumer.close();

	const result = await consumer.next();
	assert.strictEqual(result, undefined);

	producer.close();
});

// --- Latency: zero target ---

test("latency 0 delivers only the latest groups", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	// Default latency is 0 (most aggressive skipping)
	const consumer = new Consumer(track);

	// Write 10 groups with 3 frames each, 50ms group spacing.
	// Multiple frames per group create interleaving between #runGroup tasks,
	// allowing #checkLatency to fire and skip groups.
	const groupCount = 10;
	const framesPerGroup = 3;
	const groupSpacing = 50_000 as Time.Micro;
	const frameSpacing = 5_000 as Time.Micro;

	for (let g = 0; g < groupCount; g++) {
		writeGroup(producer, g, framesPerGroup, frameSpacing, groupSpacing);
	}
	producer.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await consumeFrames(consumer, 200);

	// With latency 0, early groups should be aggressively skipped.
	// Total frames without skipping = 30. We expect significantly fewer.
	assert.ok(
		frames.length < groupCount * framesPerGroup,
		`Expected skipping with latency 0, got all ${frames.length} frames`,
	);

	consumer.close();
});

// --- Latency: skipping correctness ---

test("latency skip delivers correct groups", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 100 as Time.Milli });

	// Write 10 groups, 30ms spacing, 5 frames each, 2ms frame spacing
	const groupCount = 10;
	const framesPerGroup = 5;
	const groupSpacing = 30_000 as Time.Micro;
	const frameSpacing = 2_000 as Time.Micro;

	for (let g = 0; g < groupCount; g++) {
		writeGroup(producer, g, framesPerGroup, frameSpacing, groupSpacing);
	}
	producer.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await consumeFrames(consumer, 200);

	// Total span: group 9 starts at 270ms, last frame at 278ms.
	// For 100ms latency, we want groups where span from group start to max < 100ms.
	// All delivered frames should have timestamps within ~100ms of the latest.
	assert.ok(frames.length > 0, "Expected at least some frames");

	const maxTimestamp = Math.max(...frames.map((f) => f.timestamp));
	const minTimestamp = Math.min(...frames.map((f) => f.timestamp));
	assert.ok(maxTimestamp - minTimestamp <= 110_000, `Span ${maxTimestamp - minTimestamp}us should be <= 110ms`);

	consumer.close();
});

// --- next() API contract ---

test("next() returns undefined for group boundaries", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write 3 groups, 2 frames each
	for (let g = 0; g < 3; g++) {
		for (let f = 0; f < 2; f++) {
			const timestamp = (g * 100_000 + f * 10_000) as Time.Micro;
			encodeFrame(producer, timestamp, f === 0);
		}
	}
	producer.close();

	await new Promise((resolve) => setTimeout(resolve, 50));

	// Consume all results including group-done markers
	const allResults: { frame: boolean; group: number }[] = [];

	for (;;) {
		const result = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), 500)),
		]);

		if (result === null || result === undefined) break;
		allResults.push({ frame: result.frame !== undefined, group: result.group });
	}

	// We should see frames and group-done markers (frame: false)
	const groupDoneMarkers = allResults.filter((r) => !r.frame);
	const frameResults = allResults.filter((r) => r.frame);

	assert.strictEqual(frameResults.length, 6, `Expected 6 frames, got ${frameResults.length}`);
	assert.strictEqual(groupDoneMarkers.length, 3, `Expected 3 group-done markers, got ${groupDoneMarkers.length}`);

	// Group-done markers should have ascending group numbers
	for (let i = 1; i < groupDoneMarkers.length; i++) {
		assert.ok(groupDoneMarkers[i].group > groupDoneMarkers[i - 1].group);
	}

	consumer.close();
});

test("concurrent next() calls throw", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Don't write anything yet so next() will block waiting for notify
	const first = consumer.next();

	// Second concurrent call should throw
	await assert.rejects(() => consumer.next(), {
		message: "multiple calls to decode not supported",
	});

	// Clean up: close everything so the first promise resolves
	consumer.close();
	await first;
	producer.close();
});

// --- Frame decoding correctness ---

test("frames are decoded with correct timestamps and keyframe flags", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write 1 group with 3 frames: keyframe + 2 deltas
	encodeFrame(producer, 0 as Time.Micro, true); // keyframe
	encodeFrame(producer, 33_333 as Time.Micro, false); // delta
	encodeFrame(producer, 66_666 as Time.Micro, false); // delta
	producer.close();

	await new Promise((resolve) => setTimeout(resolve, 50));

	const decoded: { timestamp: Time.Micro; keyframe: boolean }[] = [];

	for (;;) {
		const result = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), 300)),
		]);

		if (result === null || result === undefined) break;
		if (result.frame) {
			decoded.push({ timestamp: result.frame.timestamp, keyframe: result.frame.keyframe });
		}
	}

	assert.strictEqual(decoded.length, 3);
	assert.strictEqual(decoded[0].timestamp, 0);
	assert.strictEqual(decoded[0].keyframe, true);
	assert.strictEqual(decoded[1].timestamp, 33_333);
	assert.strictEqual(decoded[1].keyframe, false);
	assert.strictEqual(decoded[2].timestamp, 66_666);
	assert.strictEqual(decoded[2].keyframe, false);

	consumer.close();
});

// --- Edge cases ---

test("empty track returns undefined", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Close the track immediately without writing anything
	producer.close();

	const result = await Promise.race([
		consumer.next(),
		new Promise<null>((resolve) => setTimeout(() => resolve(null), 300)),
	]);

	// Should return undefined (track closed) or timeout (null)
	assert.ok(result === undefined || result === null, "Expected undefined or timeout for empty track");

	consumer.close();
});

test("track closed with error propagates gracefully", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write some frames
	encodeFrame(producer, 0 as Time.Micro, true);

	await new Promise((resolve) => setTimeout(resolve, 50));

	// Close track with an error
	producer.close(new Error("test error"));

	// Consumer should handle it gracefully and return undefined
	const result = await Promise.race([
		consumer.next(),
		new Promise<null>((resolve) => setTimeout(() => resolve(null), 300)),
	]);

	// The first frame may or may not be delivered depending on timing,
	// but eventually the consumer should stop (undefined or timeout).
	if (result !== null && result !== undefined) {
		// Got a frame, try one more time - should eventually end
		const result2 = await Promise.race([
			consumer.next(),
			new Promise<null>((resolve) => setTimeout(() => resolve(null), 300)),
		]);
		assert.ok(
			result2 === undefined || result2 === null || result2.frame === undefined,
			"Expected consumer to stop after track error",
		);
	}

	consumer.close();
});

test("buffered signal updates as frames arrive and are consumed", async () => {
	const track = new Track("test");
	const producer = new Producer(track);
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Initially no buffered ranges
	assert.deepStrictEqual(consumer.buffered.peek(), []);

	// Write 3 groups with 2 frames each, well-spaced timestamps
	for (let g = 0; g < 3; g++) {
		for (let f = 0; f < 2; f++) {
			const timestamp = (g * 100_000 + f * 33_000) as Time.Micro;
			encodeFrame(producer, timestamp, f === 0);
		}
	}

	// Wait for frames to be buffered
	await new Promise((resolve) => setTimeout(resolve, 100));

	// Should have buffered ranges
	const rangesBefore = consumer.buffered.peek();
	assert.ok(rangesBefore.length > 0, "Expected buffered ranges after writing frames");
	const totalBefore = rangesBefore.reduce((sum, r) => sum + (r.end as number) - (r.start as number), 0);
	assert.ok(totalBefore > 0, "Expected non-zero buffered duration");

	producer.close();

	// Consume all frames in one go
	const frames = await consumeFrames(consumer, 500);
	assert.ok(frames.length > 0, "Expected to consume some frames");

	// After consuming all frames, buffered should be empty or smaller
	const rangesAfter = consumer.buffered.peek();
	const totalAfter = rangesAfter.reduce((sum, r) => sum + (r.end as number) - (r.start as number), 0);
	assert.ok(totalAfter <= totalBefore, "Buffered ranges should shrink after consumption");

	consumer.close();
});
