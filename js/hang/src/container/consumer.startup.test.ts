import { expect, jest, onTestFinished, spyOn, test } from "bun:test";
import { Group, Error as NetError, StreamCode, Time, Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";

// Drain the in-memory readers and their continuations without advancing mocked time.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

function setup(maxDelay = 100) {
	jest.useFakeTimers();
	const track = new Track.Producer("video");
	const consumer = new Consumer(track.subscribe({ maxDelay: Time.Milli(30_000) }), {
		format: new Format("video"),
		maxDelay: Time.Milli(maxDelay),
	});
	const results: NonNullable<Awaited<ReturnType<Consumer["next"]>>>[] = [];
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const reading = (async () => {
		for (;;) {
			const result = await consumer.next();
			if (!result) return;
			results.push(result);
		}
	})();
	onTestFinished(async () => {
		consumer.close();
		track.close();
		try {
			await reading;
		} finally {
			warn.mockRestore();
			jest.useRealTimers();
		}
	});
	return { track, consumer, results, warn };
}

function write(group: Group.Producer, timestamp: number, marker = false) {
	group.writeFrame({
		payload: encodeFrame(marker ? new Uint8Array() : new Uint8Array([1]), Time.Micro(timestamp)),
		timestamp: Time.Timestamp.now(),
	});
}

for (const emptySuccessor of [false, true]) {
	test.each([0, 100])(
		`Consumer starts across a hole after an empty reset (empty successor: ${emptySuccessor}, maxDelay: %s)`,
		async (maxDelay) => {
			const { track, consumer, results, warn } = setup(maxDelay);
			const stale = new Group.Producer(16);
			track.writeGroup(stale);
			await flush();
			if (emptySuccessor) {
				const empty = new Group.Producer(17);
				track.writeGroup(empty);
				empty.close();
				await flush();
			}
			stale.close(new NetError.Stream(StreamCode.Old));
			await flush();
			expect(results.some((result) => result.group === 16 && !result.frame)).toBe(true);
			if (emptySuccessor) expect(results.some((result) => result.group === 17 && !result.frame)).toBe(true);

			const sequence = emptySuccessor ? 19 : 18;
			const live = new Group.Producer(sequence);
			track.writeGroup(live);
			for (let i = 0; i < 5; i++) write(live, 1_000_000 + i * 33_333);
			await flush();

			const frames = results.filter((result) => result.frame);
			expect(frames.map((result) => result.group)).toEqual(Array(5).fill(sequence));
			expect(frames.map((result) => result.frame?.timestamp)).toEqual(
				[1_000_000, 1_033_333, 1_066_666, 1_099_999, 1_133_332].map((timestamp) => Time.Micro(timestamp)),
			);
			expect(frames.map((result) => result.frame?.keyframe)).toEqual([true, false, false, false, false]);
			expect(frames.map((result) => result.continuous)).toEqual([false, true, true, true, true]);
			expect(consumer.discontinuity).toBe(0);
			expect(warn).not.toHaveBeenCalled();
		},
	);
}

test("Consumer preserves a declared marker before its first media across a hole", async () => {
	const { track, consumer, results, warn } = setup();
	const marker = new Group.Producer(16);
	track.writeGroup(marker);
	write(marker, 0, true);
	marker.close();
	await flush();
	expect(consumer.discontinuity).toBe(1);

	const live = new Group.Producer(18);
	track.writeGroup(live);
	write(live, 50_000);
	await flush();
	const frames = results.filter((result) => result.frame);
	expect(frames).toHaveLength(1);
	expect(frames[0]).toMatchObject({ group: 18, discontinuity: 1, continuous: false });
	expect(frames[0].frame?.keyframe).toBe(true);
	expect(warn).not.toHaveBeenCalled();
});

test.each([false, true])("Consumer still waits for a gap after media (playhead reset: %s)", async (reset) => {
	const { track, consumer, results, warn } = setup(1000);
	const first = new Group.Producer(16);
	track.writeGroup(first);
	write(first, 0);
	first.close();
	await flush();
	expect(results.filter((result) => result.frame)).toHaveLength(1);

	if (reset) {
		const marker = new Group.Producer(17);
		track.writeGroup(marker);
		write(marker, 0, true);
		marker.close();
		await flush();
		expect(consumer.discontinuity).toBe(1);
	}
	const sequence = reset ? 20 : 19;
	const later = new Group.Producer(sequence);
	track.writeGroup(later);
	write(later, 50_000);
	await flush();
	expect(consumer.buffered.peek().length).toBeGreaterThan(0);
	expect(results.filter((result) => result.frame)).toHaveLength(1);

	// Supply the missing, timestamp-contiguous bridge and prove the queued frame survives.
	const bridge = new Group.Producer(sequence - 1);
	track.writeGroup(bridge);
	write(bridge, 0);
	bridge.close();
	await flush();
	expect(results.filter((result) => result.frame).map((result) => result.group)).toEqual([
		16,
		sequence - 1,
		sequence,
	]);
	expect(consumer.discontinuity).toBe(reset ? 1 : 0);
	expect(warn).not.toHaveBeenCalled();
});
