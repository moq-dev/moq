import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Micro, Milli, Nano } from "@moq/clock";
import { type Group, Time, Track } from "@moq/net";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";

const BUDGET = 500;

// A consumer that starts `min` plus `warmup` behind the live edge and skips at `max`, `null` for no limit.
function setup(delay = BUDGET, warmup = 0, min = delay, max: number | null = delay) {
	const clock = new Manual();
	const installed = clock.install();
	// Drain the in-memory readers and their continuations without moving time.
	const flush = () => clock.advance(Nano.zero);
	const track = new Track.Producer("video");
	// A null max delay sets no subscriber limit; an omitted one is zero.
	const consumer = new Consumer(track.subscribe({ maxDelay: max === null ? null : Milli(max + warmup) }), {
		format: new Format("video"),
		delay: { min: Milli(min), max: max === null ? null : Milli(max) },
		warmup: Milli(warmup),
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

test("the start waits for an earlier group when the live edge arrives first", async () => {
	const { track, results, flush } = setup();
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual([]);

	group(track, 4, [4_500_000]).finish();
	await flush();
	expect(frames(results)).toEqual(["4@4500000", "5@5000000"]);
});

test("a zero reach starts at a live edge that arrives first", async () => {
	const { track, results, flush } = setup(0);
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("the warmup reaches further back for the start", async () => {
	for (const [warmup, start] of [
		[0, "4@4800000"],
		[300, "3@4500000"],
	] as const) {
		const { track, results, flush } = setup(200, warmup);
		// Newest first: group 4 covers the max delay alone, but not the warmup too.
		group(track, 5, [5_000_000]).finish();
		group(track, 4, [4_800_000]).finish();
		await flush();
		group(track, 3, [4_500_000]).finish();
		await flush();
		expect(frames(results)[0]).toEqual(start);
		results.length = 0;
	}
});

test("a quiet track starts within one reach", async () => {
	const { clock, track, results, flush } = setup(200, 300);
	group(track, 5, [5_000_000]);
	await flush();
	await clock.advance(Nano.fromMilli(Milli(BUDGET - 50)));
	expect(frames(results)).toEqual([]);

	await clock.advance(Nano.fromMilli(Milli(50)));
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("the start needs no wait once a group covers the reach", async () => {
	const { track, results, flush } = setup();
	group(track, 5, [5_000_000, 5_000_000 + BUDGET * 1000]);
	await flush();
	expect(frames(results)).toEqual(["5@5000000", "5@5500000"]);
});

test("the start keeps a group that began before the reach but covers it", async () => {
	const { track, results, flush } = setup(200, 300);
	group(track, 5, [5_000_000]);
	await flush();
	// Group 4 begins 600ms before the newest content, past the 500ms reach, yet covers its 4.5s target.
	group(track, 4, [4_400_000, 4_600_000, 4_900_000]).finish();
	await flush();
	expect(frames(results)[0]).toEqual("4@4400000");
});

test("the age skip still drops a group its successor covers", async () => {
	const { track, results, flush } = setup();
	group(track, 5, [5_000_000]);
	await flush();
	// Group 6 starts a whole budget behind group 7's content, so group 5 cannot cover the reach.
	group(track, 6, [5_000_000 + (BUDGET + 100) * 1000]);
	group(track, 7, [5_000_000 + (2 * BUDGET + 200) * 1000]);
	await flush();
	expect(frames(results)[0]).toEqual("6@5600000");
});

test("a large max delay alone starts at a live edge that arrives first", async () => {
	const { track, results, flush } = setup(0, 0, 0, 10_000);
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("a min delay past the max delay reaches only the max delay", async () => {
	const { clock, track, results, flush } = setup(200, 0, 2_000);
	group(track, 5, [5_000_000]);
	await flush();
	await clock.advance(Nano.fromMilli(Milli(150)));
	expect(frames(results)).toEqual([]);

	await clock.advance(Nano.fromMilli(Milli(50)));
	expect(frames(results)).toEqual(["5@5000000"]);
});

test("an unlimited max delay still reaches the min delay", async () => {
	const { track, results, flush } = setup(BUDGET, 0, BUDGET, null);
	group(track, 5, [5_000_000]);
	await flush();
	expect(frames(results)).toEqual([]);

	group(track, 4, [4_500_000]).finish();
	await flush();
	expect(frames(results)).toEqual(["4@4500000", "5@5000000"]);
});

test("the covering group is kept while its warmup arrives after the first read", async () => {
	const { track, results, flush } = setup(200, 300);
	group(track, 5, [5_000_000]);
	await flush();
	// Group 4 covers the 4.5s target, and its first frame is read before the rest of it lands.
	const covering = group(track, 4, [4_400_000]);
	await flush();
	expect(frames(results)).toEqual(["4@4400000"]);

	for (const timestamp of [4_420_000, 4_700_000, 4_900_000]) {
		write(covering, timestamp);
		await flush();
	}
	covering.finish();
	await flush();
	expect(frames(results)).toEqual(["4@4400000", "4@4420000", "4@4700000", "4@4900000", "5@5000000"]);
});

test("the covering group is kept while it arrives after the first read, without a warmup", async () => {
	const { track, results, flush } = setup(200);
	group(track, 5, [5_000_000]);
	await flush();
	// Group 4 covers the 4.8s target, so its media before that point is older than the max delay.
	const covering = group(track, 4, [4_400_000]);
	await flush();
	expect(frames(results)).toEqual(["4@4400000"]);

	for (const timestamp of [4_433_000, 4_700_000, 4_900_000]) {
		write(covering, timestamp);
		await flush();
	}
	covering.finish();
	await flush();
	expect(frames(results)).toEqual(["4@4400000", "4@4433000", "4@4700000", "4@4900000", "5@5000000"]);
});

test("an age skip that ends the start still warms until the start's target", async () => {
	const { track, results, flush } = setup(200, 300);
	// Group 4 alone does not cover its 4.5s target, so the start waits.
	group(track, 4, [5_000_000]);
	await flush();
	// Groups 5 and 6 decode in one pass: group 6's 6.2s edge puts the 500ms reach's target at 5.7s,
	// and the age skip drops group 4, whose successor starts past the reach, before the start sees it.
	const covering = group(track, 5, []);
	const newest = group(track, 6, []);
	await flush();
	write(covering, 5_600_000);
	write(covering, 5_900_000);
	write(newest, 6_200_000);
	await flush();
	expect(frames(results)).toEqual(["5@5600000", "5@5900000"]);

	// The reader warms until 6.0s (the target plus the warmup), not 5.9s (the first frame plus the
	// warmup), so newer content does not yet drop the covering group.
	write(newest, 6_210_000);
	await flush();
	write(covering, 5_950_000);
	covering.finish();
	await flush();
	expect(frames(results)).toEqual(["5@5600000", "5@5900000", "5@5950000", "6@6200000", "6@6210000"]);
});
