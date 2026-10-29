import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Micro, Milli, Nano } from "@moq/clock";
import { type Group, Error as NetError, StreamCode, Time, Track } from "@moq/net";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";

function setup(maxDelay = 100) {
	const clock = new Manual();
	const installed = clock.install();
	// Drain the in-memory readers and their continuations without moving time.
	const flush = () => clock.advance(Nano.zero);
	const track = new Track.Producer("video");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("video"),
		maxDelay: Milli(maxDelay),
	});
	const results: Read[] = [];
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
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			warn.mockRestore();
			installed.close();
		}
	});
	return { track, consumer, results, warn, flush };
}

// Each item as "gap <µs>+<µs>", "<group>@<µs>" for a frame, or "end <group>@<µs>".
function items(results: Read[]): string[] {
	return results.map((read) => {
		if (read.kind === "gap") return `gap ${read.timestamp}+${read.duration}`;
		if (read.kind === "end") return `end ${read.group}@${read.end}`;
		return `${read.group}@${read.frame.timestamp}`;
	});
}

function frames(results: Read[]) {
	return results.flatMap((read) => (read.kind === "frame" ? [read] : []));
}

function write(group: Group.Producer, timestamp: number, marker = false) {
	group.writeFrame({
		payload: encodeFrame(marker ? new Uint8Array() : new Uint8Array([1]), Micro(timestamp)),
		timestamp: Time.Timestamp.now(),
	});
}

for (const emptySuccessor of [false, true]) {
	test.each([0, 100])(
		`Consumer starts across a hole after an empty reset (empty successor: ${emptySuccessor}, maxDelay: %s)`,
		async (maxDelay) => {
			const { track, results, warn, flush } = setup(maxDelay);
			const stale = track.createGroup(16);
			await flush();
			if (emptySuccessor) {
				const empty = track.createGroup(17);
				empty.finish();
				await flush();
			}
			stale.abort(new NetError.Stream(StreamCode.Old));
			await flush();
			expect(results).toEqual([]);

			const sequence = emptySuccessor ? 19 : 18;
			const live = track.createGroup(sequence);
			for (let i = 0; i < 5; i++) write(live, 1_000_000 + i * 33_333);
			await flush();

			// Nothing was read before the live group, so the stale one leaves no gap.
			expect(items(results)).toEqual([
				...[1_000_000, 1_033_333, 1_066_666, 1_099_999, 1_133_332].map(
					(timestamp) => `${sequence}@${timestamp}`,
				),
			]);
			expect(frames(results).map((read) => read.frame.keyframe)).toEqual([true, false, false, false, false]);
			expect(warn).not.toHaveBeenCalled();
		},
	);
}

test("Consumer preserves a declared marker before its first media across a hole", async () => {
	const { track, results, warn, flush } = setup();
	const marker = track.createGroup(16);
	write(marker, 0, true);
	marker.finish();
	await flush();
	expect(items(results)).toEqual(["end 16@0"]);

	const live = track.createGroup(18);
	write(live, 50_000);
	await flush();
	// Group 17 never arrived, so the media after the marker is past a gap. It spans the declared
	// pause too: the reader can't tell where group 17 would have started.
	expect(items(results)).toEqual(["end 16@0", "gap 0+50000", "18@50000"]);
	expect(frames(results)[0].frame.keyframe).toBe(true);
	expect(warn).not.toHaveBeenCalled();
});

test.each([false, true])("Consumer still waits for a gap after media (playhead reset: %s)", async (reset) => {
	const { track, consumer, results, warn, flush } = setup(1000);
	const first = track.createGroup(16);
	write(first, 0);
	first.finish();
	await flush();
	expect(items(results)).toEqual(["16@0"]);

	if (reset) {
		const marker = track.createGroup(17);
		write(marker, 0, true);
		marker.finish();
		await flush();
		expect(items(results)).toEqual(["16@0", "end 17@0"]);
	}
	const sequence = reset ? 20 : 19;
	const later = track.createGroup(sequence);
	write(later, 50_000);
	await flush();
	expect(consumer.buffered.peek().length).toBeGreaterThan(0);
	expect(frames(results)).toHaveLength(1);

	// Supply the missing, timestamp-contiguous bridge and prove the queued frame survives.
	const bridge = track.createGroup(sequence - 1);
	write(bridge, 0);
	bridge.finish();
	await flush();
	// The bridge closes the hole, so nothing reads as a gap.
	expect(items(results)).toEqual(["16@0", ...(reset ? ["end 17@0"] : []), `${sequence - 1}@0`, `${sequence}@50000`]);
	expect(warn).not.toHaveBeenCalled();
});
