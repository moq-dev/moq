import { expect, test } from "bun:test";
import { Manual, Micro, Milli, Nano, Second } from "@moq/clock";
import { now, pace, type Sample, SEEK_BEHIND, type Sink } from "./pace";
import { Timeline } from "./timeline";

// A ten-second file with a sample every 100ms.
const DURATION = 10;
const SPACING = 100_000;

class FakeSample implements Sample {
	microsecondTimestamp: number;
	restamped?: number;
	closed = false;

	constructor(timestamp: number) {
		this.microsecondTimestamp = timestamp;
	}

	setTimestamp(seconds: number) {
		this.restamped = Math.round(seconds * 1_000_000);
	}

	close() {
		this.closed = true;
	}
}

// Records where each pass starts and every sample it decodes. Like mediabunny, a start position
// begins at the sample covering it.
function fakeSink() {
	const starts: (number | undefined)[] = [];
	const decoded: FakeSample[] = [];
	const sink: Sink<FakeSample> = {
		async *samples(start?: number) {
			starts.push(start);
			const first = Math.floor(Math.round((start ?? 0) * 1_000_000) / SPACING);
			for (let i = first; i * SPACING < DURATION * 1_000_000; i++) {
				const sample = new FakeSample(i * SPACING);
				decoded.push(sample);
				yield sample;
			}
		},
	};
	return { sink, starts, decoded };
}

function setup() {
	const clock = new Manual();
	const installed = clock.install();
	// Start away from zero so an unanchored timestamp can't pass by accident.
	clock.jump(Nano.fromSecond(Second(1)));

	const { sink, starts, decoded } = fakeSink();
	const zero = now();
	const abort = new AbortController();
	const reader = pace(sink, new Timeline(zero, DURATION), (sample) => sample.restamped, abort.signal).getReader();

	const read = async () => (await clock.drive(reader.read())).value;
	const close = async () => {
		abort.abort();
		await reader.cancel();
		installed.close();
	};
	return { clock, zero, read, starts, decoded, close };
}

test("a reader returning after a long gap seeks to the present on the shared timeline", async () => {
	const { clock, zero, read, starts, decoded, close } = setup();
	try {
		expect(await read()).toBe(zero);

		// Unread for a minute and a bit: six full passes, then 1.55s into the seventh.
		clock.jump(Nano.fromMilli(Milli(61_550)));

		// The sample covering the present, on the pass the clock is in, rather than the old cursor's.
		expect(await read()).toBe(zero + 61_500_000);
		expect(starts).toEqual([undefined, 1.55]);
		// Only the first stale sample was decoded before seeking, not the minute that was missed.
		expect(decoded.map((sample) => sample.microsecondTimestamp)).toEqual([0, SPACING, 1_500_000]);

		// Pacing continues from there at 1x.
		expect(await read()).toBe(zero + 61_600_000);
		expect(Nano.toMicro(Nano(clock.now()))).toBe(Micro(zero + 61_600_000));
	} finally {
		await close();
	}
	expect(decoded.every((sample) => sample.closed)).toBe(true);
});

test("a briefly slow reader drains every sample without seeking", async () => {
	const { clock, zero, read, starts, close } = setup();
	try {
		expect(await read()).toBe(zero);

		// Behind, but by less than the seek threshold.
		clock.jump(Micro.toNano(Micro(SEEK_BEHIND - SPACING)));

		const late = [];
		for (let i = 1; i <= 5; i++) late.push(await read());
		expect(late).toEqual([1, 2, 3, 4, 5].map((i) => zero + i * SPACING));
		expect(starts).toEqual([undefined]);
	} finally {
		await close();
	}
});
