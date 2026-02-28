import assert from "node:assert";
import test from "node:test";
import { Group, Time, Track, Varint } from "@moq/lite";
import { Consumer, Producer } from "./legacy.ts";

// Helper: encode a frame using the legacy container format (varint timestamp + payload).
function encodeFrame(producer: Producer, timestamp: Time.Micro, keyframe: boolean) {
	producer.encode(new Uint8Array([0xde, 0xad]), timestamp, keyframe);
}

// Helper: encode a raw frame in legacy container format (varint timestamp + payload).
function encodeLegacyFrame(timestamp: Time.Micro): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const payload = new Uint8Array([0xde, 0xad]);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

// Helper: write a group with a specific sequence number directly to a track.
function writeGroupWithSequence(track: Track, sequence: number, timestamps: Time.Micro[]) {
	const group = new Group(sequence);
	for (const ts of timestamps) {
		group.writeFrame(encodeLegacyFrame(ts));
	}
	group.close();
	track.writeGroup(group);
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

test("consumer recovers from gap in group sequence numbers", async () => {
	const track = new Track("test");
	// Latency target of 100ms. Groups after the gap span >100ms so #checkLatency fires.
	const consumer = new Consumer(track, { latency: 100 as Time.Milli });

	// Write groups 0 and 1 normally.
	writeGroupWithSequence(track, 0, [0 as Time.Micro, 20_000 as Time.Micro]);
	writeGroupWithSequence(track, 1, [40_000 as Time.Micro, 60_000 as Time.Micro]);

	// Skip group 2 entirely (simulating a dropped group).
	// Write groups 3-6, spanning 120ms-260ms (140ms span > 100ms latency target).
	writeGroupWithSequence(track, 3, [120_000 as Time.Micro, 140_000 as Time.Micro]);
	writeGroupWithSequence(track, 4, [160_000 as Time.Micro, 180_000 as Time.Micro]);
	writeGroupWithSequence(track, 5, [200_000 as Time.Micro, 220_000 as Time.Micro]);
	writeGroupWithSequence(track, 6, [240_000 as Time.Micro, 260_000 as Time.Micro]);

	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	// The bug: after consuming group 1, #active becomes 2. But group 2 never arrives,
	// so next() waits forever because groups[0].sequence (3) > #active (2).
	// The fix: #checkLatency sees the span exceeds 100ms, advances #active past the
	// gap, and skips old groups until within the latency target.
	const frames = await consumeFrames(consumer, 500);

	// All groups arrive at once, so the full span (0-260ms) exceeds the 100ms target.
	// #checkLatency skips old groups (including past the gap) until within budget.
	// The important thing: the consumer does NOT deadlock on the missing group 2.
	assert.ok(frames.length >= 4, `Expected >= 4 frames, got ${frames.length}`);

	consumer.close();
});

test("consumer recovers from gap at the start of sequence numbers", async () => {
	const track = new Track("test");
	// Latency target of 80ms. Groups span >80ms so #checkLatency fires.
	const consumer = new Consumer(track, { latency: 80 as Time.Milli });

	// First group has sequence 5 (simulating joining a stream mid-way).
	// Group 6 is missing. Groups 7-10 arrive, spanning enough to trigger latency skip.
	writeGroupWithSequence(track, 5, [0 as Time.Micro, 20_000 as Time.Micro]);
	writeGroupWithSequence(track, 7, [80_000 as Time.Micro, 100_000 as Time.Micro]);
	writeGroupWithSequence(track, 8, [120_000 as Time.Micro, 140_000 as Time.Micro]);
	writeGroupWithSequence(track, 9, [160_000 as Time.Micro, 180_000 as Time.Micro]);

	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await consumeFrames(consumer, 500);

	// Group 5 is consumed normally (2 frames). Then #active = 6 (missing).
	// Groups 7-9 accumulate, span = 100ms > 80ms, so latency skip fires.
	assert.ok(frames.length >= 4, `Expected >= 4 frames, got ${frames.length}`);

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

test("buffered merges consecutive done groups into one range", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write 3 sequential groups, each with a single frame (like audio).
	// Without merging, each would be a zero-width point range.
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);
	writeGroupWithSequence(track, 1, [23_000 as Time.Micro]);
	writeGroupWithSequence(track, 2, [46_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 100));

	// All groups are done (fully received). Since they have consecutive sequence
	// numbers, they should merge into a single contiguous range.
	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 1, `Expected 1 merged range, got ${ranges.length}: ${JSON.stringify(ranges)}`);
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 46);

	consumer.close();
	track.close();
});

test("buffered shows gap when group sequence numbers are missing", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write groups 0, 1, and 3 (skipping 2).
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);
	writeGroupWithSequence(track, 1, [23_000 as Time.Micro]);
	// group 2 missing
	writeGroupWithSequence(track, 3, [69_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 100));

	// Groups 0 and 1 should merge (consecutive, done).
	// Group 3 should be a separate range (gap at group 2).
	const ranges = consumer.buffered.peek();
	assert.strictEqual(
		ranges.length,
		2,
		`Expected 2 ranges (gap at group 2), got ${ranges.length}: ${JSON.stringify(ranges)}`,
	);

	// First range: groups 0-1
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 23);

	// Second range: group 3
	assert.strictEqual(ranges[1].start, 69);
	assert.strictEqual(ranges[1].end, 69);

	consumer.close();
	track.close();
});

test("buffered does not merge non-consecutive groups across a gap", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Groups 0 and 2 (gap at 1). Even though both are done, they shouldn't merge.
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);
	writeGroupWithSequence(track, 2, [46_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 100));

	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 2, `Expected 2 ranges (gap at group 1), got ${ranges.length}`);

	// Group 0 range: [0, 0]. No extension because group 1 (N+1) is missing.
	// N+2 (group 2) exists but is not enough — extension requires exactly N+1.
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 0);
	assert.strictEqual(ranges[1].start, 46);
	assert.strictEqual(ranges[1].end, 46);

	consumer.close();
	track.close();
});

// --- Buffered ranges: in-progress groups ---

test("buffered range for in-progress group uses last frame as end", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write a group with 2 frames but don't close it.
	const group = new Group(0);
	group.writeFrame(encodeLegacyFrame(0 as Time.Micro));
	group.writeFrame(encodeLegacyFrame(33_000 as Time.Micro));
	track.writeGroup(group);
	// group is NOT closed — still in progress

	await new Promise((resolve) => setTimeout(resolve, 100));

	// Group is in-progress, so the last frame has duration 0.
	// Range should be [0ms, 33ms].
	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 1, `Expected 1 range, got ${ranges.length}`);
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 33);

	group.close();
	consumer.close();
	track.close();
});

test("buffered extension applies when N is done and N+1 has first frame but is not done", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Group 0: done (closed), single frame at 0ms.
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);

	// Group 1: in progress (not closed), first frame at 50ms.
	const group1 = new Group(1);
	group1.writeFrame(encodeLegacyFrame(50_000 as Time.Micro));
	track.writeGroup(group1);

	await new Promise((resolve) => setTimeout(resolve, 100));

	// Group 0 is done (FIN received) and consecutive group 1 has its first frame.
	// Extension applies: group 0's last frame duration extends to group 1's first
	// frame timestamp (50ms). Group 0 range: [0, 50]. Group 1 range: [50, 50].
	// Merged: [0, 50].
	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 1, `Expected 1 range, got ${ranges.length}`);
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 50);

	group1.close();
	consumer.close();
	track.close();
});

test("no buffered extension when N+1 exists but has no frames", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Group 0: done, frame at 0ms.
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);

	// Group 1: exists on the track but has no frames yet.
	const group1 = new Group(1);
	track.writeGroup(group1);

	await new Promise((resolve) => setTimeout(resolve, 100));

	// Group 0 is done and group 1 exists, but group 1 has no frames.
	// Without a first frame timestamp, we can't compute the extension.
	// Group 0 range: [0, 0] (no extension).
	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 1, `Expected 1 range, got ${ranges.length}`);
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 0);

	group1.close();
	consumer.close();
	track.close();
});

// --- Buffered ranges: dynamic updates ---

test("late group arrival fills gap in buffered ranges", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write groups 0 and 2 (gap at 1).
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);
	writeGroupWithSequence(track, 2, [46_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 50));

	// Should have 2 ranges (gap at group 1).
	const rangesBefore = consumer.buffered.peek();
	assert.strictEqual(rangesBefore.length, 2, `Expected 2 ranges before gap fill, got ${rangesBefore.length}`);

	// Now fill the gap with group 1.
	writeGroupWithSequence(track, 1, [23_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 50));

	// Gap is filled: all 3 groups consecutive and done.
	// Group 0 extends to group 1 start (23ms), group 1 extends to group 2 start (46ms).
	// Merged: [0, 46].
	const rangesAfter = consumer.buffered.peek();
	assert.strictEqual(rangesAfter.length, 1, `Expected 1 range after gap fill, got ${rangesAfter.length}`);
	assert.strictEqual(rangesAfter[0].start, 0);
	assert.strictEqual(rangesAfter[0].end, 46);

	consumer.close();
	track.close();
});

// --- Buffered ranges: B-frames ---

test("buffered range handles B-frames (out-of-order timestamps within group)", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// B-frame decode order: I-frame at 0, P-frame at 33ms, B-frame at 16ms.
	// Timestamps within the group are out of order, but that's valid for B-frames.
	// The group's buffered range should be [min, max] = [0, 33].
	writeGroupWithSequence(track, 0, [0 as Time.Micro, 33_000 as Time.Micro, 16_000 as Time.Micro]);

	await new Promise((resolve) => setTimeout(resolve, 100));

	const ranges = consumer.buffered.peek();
	assert.strictEqual(ranges.length, 1);
	assert.strictEqual(ranges[0].start, 0);
	assert.strictEqual(ranges[0].end, 33);

	consumer.close();
	track.close();
});

// --- Group ordering ---

test("consumer delivers groups in sequence order regardless of write order", async () => {
	const track = new Track("test");
	const consumer = new Consumer(track, { latency: 500 as Time.Milli });

	// Write groups out of sequence order.
	writeGroupWithSequence(track, 2, [60_000 as Time.Micro]);
	writeGroupWithSequence(track, 0, [0 as Time.Micro]);
	writeGroupWithSequence(track, 1, [30_000 as Time.Micro]);

	track.close();

	await new Promise((resolve) => setTimeout(resolve, 100));

	const frames = await consumeFrames(consumer, 500);
	assert.strictEqual(frames.length, 3, `Expected 3 frames, got ${frames.length}`);

	// Should be delivered in group sequence order, not write order.
	assert.strictEqual(frames[0].group, 0);
	assert.strictEqual(frames[1].group, 1);
	assert.strictEqual(frames[2].group, 2);

	consumer.close();
});
