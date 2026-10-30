import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Micro, Milli, Nano } from "@moq/clock";
import { type Group, Time, Track } from "@moq/net";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";

const BUDGET = 500;

function setup(lookback: number) {
	const clock = new Manual();
	const installed = clock.install();
	// Drain the in-memory readers and their continuations without moving time.
	const flush = () => clock.advance(Nano.zero);
	const track = new Track.Producer("video");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("video"),
		maxDelay: Milli(BUDGET),
		lookback,
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
	return { clock, track, results, flush };
}

// Each frame as "<group>@<µs>".
function frames(results: Read[]): string[] {
	return results.flatMap((read) => (read.kind === "frame" ? [`${read.group}@${read.frame.timestamp}`] : []));
}

function write(group: Group.Producer, timestamp: number) {
	group.writeFrame({
		payload: encodeFrame(new Uint8Array([1]), Micro(timestamp)),
		timestamp: Time.Timestamp.now(),
	});
}

function group(track: Track.Producer, sequence: number, timestamps: number[]): Group.Producer {
	const group = track.createGroup(sequence);
	for (const timestamp of timestamps) write(group, timestamp);
	return group;
}

test("lookback waits for the group below a live edge that arrives first", async () => {
	const { track, results, flush } = setup(1);
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual([]);

	group(track, 4, [4_800_000]).finish();
	await flush();
	expect(frames(results)).toEqual(["4@4800000", "5@5000000"]);
});

test("without lookback a live edge that arrives first is the start", async () => {
	const { track, results, flush } = setup(0);
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("lookback on a quiet track starts within one budget", async () => {
	const { clock, track, results, flush } = setup(1);
	group(track, 5, [5_000_000]);
	await flush();
	await clock.advance(Nano.fromMilli(Milli(BUDGET - 50)));
	expect(frames(results)).toEqual([]);

	await clock.advance(Nano.fromMilli(Milli(50)));
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("lookback ends once the newest content is a budget past its start", async () => {
	const { track, results, flush } = setup(1);
	group(track, 5, [5_000_000, 5_000_000 + BUDGET * 1000]);
	await flush();
	expect(frames(results)).toEqual(["5@5000000", "5@5500000"]);
});
