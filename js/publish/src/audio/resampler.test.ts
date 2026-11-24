import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";
import { Resampler } from "./resampler";

const micro = (value: number): Duration => Duration.micros(value);

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
	// The whole point: 44.1kHz is the most common file rate and the one Opus cannot carry. What came
	// out, plus the lookahead still owed, is exactly the input's duration at the new rate.
	test("converts 44100 to 48000 at the right rate", () => {
		const resampler = new Resampler({ from: 44_100, to: 48_000, channels: 1 });

		let output = 0;
		for (let chunk = 0; chunk < 44; chunk++) {
			output += resampler.push(tone(1000, 44_100, 440, chunk * 1000))?.channels[0].length ?? 0;
		}

		expect(output + resampler.pending).toBeCloseTo((44_000 * 48_000) / 44_100, 6);
	});

	test("converts 48000 down to 24000 at half the samples", () => {
		const resampler = new Resampler({ from: 48_000, to: 24_000, channels: 1 });

		let output = 0;
		for (let chunk = 0; chunk < 10; chunk++) {
			output += resampler.push(tone(960, 48_000, 440, chunk * 960))?.channels[0].length ?? 0;
		}

		expect(output + resampler.pending).toBe(4_800);
	});

	// Converting down filters what the output cannot carry rather than folding it into the band: an
	// 18kHz tone at 24kHz would alias to 6kHz.
	test("filters out what a lower rate cannot carry", () => {
		const resampler = new Resampler({ from: 48_000, to: 24_000, channels: 1 });
		const output = resampler.push(tone(48_000, 48_000, 18_000, 0))?.channels[0] ?? new Float32Array();
		const peak = output.subarray(1_000).reduce((max, x) => Math.max(max, Math.abs(x)), 0);
		expect(peak).toBeLessThan(1e-3);
	});

	// The drift loop steers a capture's ratio by up to 2000 ppm, which sweeps every fractional
	// position. Each 10ms of output must keep the tone's level within 0.1 dB to 20kHz, the top of
	// Opus's fullband: linear interpolation dipped 6 dB at 16kHz and 12 dB at 20kHz here.
	test("keeps the passband flat while the ratio is steered", () => {
		const rate = 48_000;
		for (const relative of [1.00005, 1.002, 1 / 1.002]) {
			for (const freq of [1_000, 8_000, 16_000, 20_000]) {
				const resampler = new Resampler({ from: rate, to: rate, channels: 1 });
				resampler.adjust(relative);
				const output: number[] = [];
				for (let offset = 0; offset < rate; offset += 128) {
					output.push(...(resampler.push(tone(128, rate, freq, offset))?.channels[0] ?? []));
				}

				// Fit the tone at each output sample's input position, a 10ms window at a time.
				const window = rate / 100;
				for (let start = window; start + window <= output.length; start += window) {
					let [ss, sc, cc, ys, yc] = [0, 0, 0, 0, 0];
					for (let n = start; n < start + window; n++) {
						const phase = (2 * Math.PI * freq * (n / relative)) / rate;
						const [sin, cos] = [Math.sin(phase), Math.cos(phase)];
						[ss, sc, cc, ys, yc] = [
							ss + sin * sin,
							sc + sin * cos,
							cc + cos * cos,
							ys + output[n] * sin,
							yc + output[n] * cos,
						];
					}
					const det = ss * cc - sc * sc;
					const gain = Math.hypot((ys * cc - yc * sc) / det, (yc * ss - ys * sc) / det);
					expect(Math.abs(20 * Math.log10(gain))).toBeLessThan(0.1);
				}
			}
		}
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

	// Between equal rates at the nominal ratio, every output sits on an input sample: the samples pass
	// through unchanged, behind the kernel's lookahead.
	test("passes equal rates through", () => {
		const resampler = new Resampler({ from: 48_000, to: 48_000, channels: 1 });
		const input = tone(1_000, 48_000, 440, 0).channels[0];
		const output = resampler.convert([input])?.[0] ?? new Float32Array();

		expect(output.length + resampler.pending).toBe(1_000);
		for (let i = 0; i < output.length; i++) expect(output[i]).toBeCloseTo(input[i], 6);
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

	// A big upsample ratio means the first chunks can't complete a sample yet, since each output reads
	// 12 input samples ahead; that must not throw or desync the running counters.
	test("survives chunks too short to complete a sample", () => {
		const resampler = new Resampler({ from: 8_000, to: 48_000, channels: 1 });

		const first = resampler.push({ timestamp: micro(0), channels: [new Float32Array(12)] });
		expect(first).toBeUndefined();

		const second = resampler.push({ timestamp: micro(1_500), channels: [Float32Array.from([2])] });
		expect(second?.channels[0].length).toBe(6);
		expect(second?.timestamp.asMicros()).toBe(0);
	});
});
