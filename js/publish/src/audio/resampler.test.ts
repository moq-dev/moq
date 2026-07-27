import { describe, expect, test } from "bun:test";
import type { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Resampler } from "./resampler";

const micro = (value: number): Time.Micro => value as Time.Micro;

// A chunk of `samples` frames of a sine at `freq` Hz, starting at input sample `offset`.
function tone(samples: number, rate: number, freq: number, offset: number, channels = 1): AudioFrame {
	return {
		timestamp: micro(Math.round((offset / rate) * 1_000_000)),
		channels: Array.from({ length: channels }, () =>
			Float32Array.from({ length: samples }, (_, i) => Math.sin((2 * Math.PI * freq * (offset + i)) / rate)),
		),
	};
}

describe("Resampler", () => {
	// The whole point: 44.1kHz is the most common file rate and the one Opus cannot carry.
	test("converts 44100 to 48000 at the right rate", () => {
		const resampler = new Resampler({ from: 44_100, to: 48_000, channels: 1 });

		let output = 0;
		for (let chunk = 0; chunk < 44; chunk++) {
			output += resampler.push(tone(1000, 44_100, 440, chunk * 1000))?.channels[0].length ?? 0;
		}

		// 44000 input samples at 44.1kHz is ~0.9977s, which is ~47891 samples at 48kHz.
		expect(output).toBeGreaterThan(47_800);
		expect(output).toBeLessThanOrEqual(47_891);
	});

	test("converts 48000 down to 24000 at half the samples", () => {
		const resampler = new Resampler({ from: 48_000, to: 24_000, channels: 1 });

		let output = 0;
		for (let chunk = 0; chunk < 10; chunk++) {
			output += resampler.push(tone(960, 48_000, 440, chunk * 960))?.channels[0].length ?? 0;
		}

		expect(output).toBeGreaterThan(4_795);
		expect(output).toBeLessThanOrEqual(4_800);
	});

	// The bug this class exists to avoid: resampling each chunk independently leaves a
	// discontinuity at every seam, which is audible as a periodic tick.
	test("is continuous across chunk boundaries", () => {
		const chunked = new Resampler({ from: 44_100, to: 48_000, channels: 1 });
		const whole = new Resampler({ from: 44_100, to: 48_000, channels: 1 });

		const out: number[] = [];
		for (let chunk = 0; chunk < 8; chunk++) {
			const frame = chunked.push(tone(128, 44_100, 440, chunk * 128));
			if (frame) out.push(...frame.channels[0]);
		}

		// The same signal pushed as one chunk has to produce the same samples.
		const single = whole.push(tone(8 * 128, 44_100, 440, 0));
		const expected = [...(single?.channels[0] ?? [])];

		expect(out.length).toBe(expected.length);
		for (let i = 0; i < out.length; i++) {
			expect(out[i]).toBeCloseTo(expected[i], 5);
		}
	});

	// Interpolating a straight line must reproduce that line, which catches an off-by-one in the
	// sample indexing that a sine wave would hide in its curvature.
	test("reproduces a linear ramp exactly", () => {
		const resampler = new Resampler({ from: 2, to: 4, channels: 1 });

		const frame = resampler.push({ timestamp: micro(0), channels: [Float32Array.from([0, 1, 2, 3])] });

		// Output positions 0, 0.5, 1, 1.5, 2, 2.5 in input samples; 3.0 needs a 5th input sample.
		expect([...(frame?.channels[0] ?? [])]).toEqual([0, 0.5, 1, 1.5, 2, 2.5]);
	});

	test("preserves every channel", () => {
		const resampler = new Resampler({ from: 44_100, to: 48_000, channels: 2 });

		const frame = resampler.push(tone(1000, 44_100, 440, 0, 2));
		expect(frame?.channels.length).toBe(2);
		expect(frame?.channels[0]).toEqual(frame?.channels[1] as Float32Array);
	});

	test("rejects a chunk with the wrong channel count", () => {
		const resampler = new Resampler({ from: 44_100, to: 48_000, channels: 2 });
		expect(() => resampler.push(tone(100, 44_100, 440, 0, 1))).toThrow("wrong number of channels");
	});

	// A big upsample ratio means the first chunks can't complete a sample yet; that must not throw
	// or desync the running counters.
	test("survives chunks too short to complete a sample", () => {
		const resampler = new Resampler({ from: 8_000, to: 48_000, channels: 1 });

		const first = resampler.push({ timestamp: micro(0), channels: [Float32Array.from([1])] });
		expect(first).toBeUndefined();

		const second = resampler.push({ timestamp: micro(125), channels: [Float32Array.from([2])] });
		expect(second?.channels[0].length).toBe(6);
	});
});
