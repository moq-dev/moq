import { expect, test } from "bun:test";
import { Duration, Instant, now, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { pace, type Sample, SEEK_BEHIND, type Sink } from "./pace";
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

// What the fake converter hands out: the restamped time, plus whether it was released.
class Converted {
	readonly at: number | undefined;
	closed = false;

	constructor(sample: FakeSample) {
		this.at = sample.restamped;
	}

	close() {
		this.closed = true;
	}
}

// Records where each pass starts and every sample it decodes. Like mediabunny, a start position
// begins at the sample covering it. A zero duration makes a track with no samples.
function fakeSink(duration = DURATION) {
	const starts: (number | undefined)[] = [];
	const decoded: FakeSample[] = [];
	const sink: Sink<FakeSample> = {
		async *samples(start?: number) {
			starts.push(start);
			const first = Math.floor(Math.round((start ?? 0) * 1_000_000) / SPACING);
			for (let i = first; i * SPACING < duration * 1_000_000; i++) {
				const sample = new FakeSample(i * SPACING);
				decoded.push(sample);
				yield sample;
			}
		},
	};
	return { sink, starts, decoded };
}

function setup(opts?: { samples?: number; convert?: (sample: FakeSample) => Converted }) {
	const clock = new Manual();
	const installed = clock.install();
	// Start away from zero so an unanchored timestamp can't pass by accident.
	clock.jump(Duration.secs(1));

	const { sink, starts, decoded } = fakeSink(opts?.samples ?? DURATION);
	// The anchor in microseconds, as restamped samples read.
	const zero = 1_000_000;
	const abort = new AbortController();
	const converted: Converted[] = [];
	const convert = opts?.convert ?? ((sample: FakeSample) => new Converted(sample));
	const reader = pace(
		sink,
		new Timeline(now(), DURATION),
		(sample) => {
			const value = convert(sample);
			converted.push(value);
			return value;
		},
		abort.signal,
	).getReader();

	const read = async () => (await clock.drive(reader.read())).value?.at;
	const close = async () => {
		abort.abort();
		// A stream that already errored rejects its cancel with that error.
		await reader.cancel().catch(() => {});
		installed.close();
	};
	return { clock, zero, reader, read, starts, decoded, converted, close };
}

test("a reader returning after a long gap seeks to the present on the shared timeline", async () => {
	const { clock, zero, read, starts, decoded, close } = setup();
	try {
		expect(await read()).toBe(zero);

		// Unread for a minute and a bit: six full passes, then 1.55s into the seventh.
		clock.jump(Duration.millis(61_550));

		// The sample covering the present, on the pass the clock is in, rather than the old cursor's.
		expect(await read()).toBe(zero + 61_500_000);
		expect(starts).toEqual([undefined, 1.55]);
		// Only the first stale sample was decoded before seeking, not the minute that was missed.
		expect(decoded.map((sample) => sample.microsecondTimestamp)).toEqual([0, SPACING, 1_500_000]);

		// Pacing continues from there at 1x.
		expect(await read()).toBe(zero + 61_600_000);
		expect(clock.now()).toEqual(Instant.ZERO.saturatingAdd(Duration.micros(zero + 61_600_000)));
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
		clock.jump(SEEK_BEHIND.sub(Duration.micros(SPACING)));

		const late = [];
		for (let i = 1; i <= 5; i++) late.push(await read());
		expect(late).toEqual([1, 2, 3, 4, 5].map((i) => zero + i * SPACING));
		expect(starts).toEqual([undefined]);
	} finally {
		await close();
	}
});

test("a looping file's empty track ends instead of restarting forever", async () => {
	const { reader, starts, close } = setup({ samples: 0 });
	try {
		expect((await reader.read()).done).toBe(true);
		expect(starts).toEqual([undefined]);
	} finally {
		await close();
	}
});

test("a sample whose conversion fails is still closed", async () => {
	const { reader, decoded, close } = setup({
		convert: () => {
			throw new Error("convert failed");
		},
	});
	try {
		await expect(reader.read()).rejects.toThrow("convert failed");
		expect(decoded.map((sample) => sample.closed)).toEqual([true]);
	} finally {
		await close();
	}
});

test("cancelling a stream stops the pending wait and closes the waiting sample", async () => {
	const { reader, read, decoded, converted, close } = setup();
	try {
		await read();

		// The next sample waits 100ms for its presentation time, which never comes.
		const pending = reader.read();
		while (decoded.length < 2) await yieldNow();

		await reader.cancel();
		expect((await pending).done).toBe(true);
		for (let i = 0; i < 5 && !decoded[1]?.closed; i++) await yieldNow();
		expect(decoded.map((sample) => sample.closed)).toEqual([true, true]);
		expect(converted.length).toBe(1);
	} finally {
		await close();
	}
});
