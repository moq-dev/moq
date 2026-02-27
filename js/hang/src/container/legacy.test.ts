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
