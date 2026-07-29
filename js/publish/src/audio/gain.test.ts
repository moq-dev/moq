import { describe, expect, test } from "bun:test";
import type { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Gain } from "./gain";

const RATE = 48_000;

// Half the rate covers the same fade in half the samples, so the ramp has to move twice as fast.
const SLOW_RATE = RATE / 2;

function ones(samples: number, channels = 1): AudioFrame {
	return {
		timestamp: 0 as Time.Micro,
		channels: Array.from({ length: channels }, () => new Float32Array(samples).fill(1)),
	};
}

describe("Gain", () => {
	test("leaves audio untouched at unity", () => {
		const gain = new Gain();
		const frame = ones(128);

		gain.set(1);
		const out = gain.apply(frame, RATE);

		expect([...out.channels[0]]).toEqual(Array.from({ length: 128 }, () => 1));
	});

	// Jumping straight to the target is what clicks, so the level has to walk there.
	test("ramps toward a mute rather than jumping", () => {
		const gain = new Gain();
		const frame = ones(128);

		gain.set(0);
		const out = gain.apply(frame, RATE);

		// 128 samples is far less than the 0.2s fade, so it has barely moved.
		expect(out.channels[0][0]).toBeLessThan(1);
		expect(out.channels[0][0]).toBeGreaterThan(0.99);
		expect(out.channels[0][127]).toBeLessThan(out.channels[0][0]);
		expect(out.channels[0][127]).toBeGreaterThan(0.98);

		// The input is shared with every other rendition, so it must come back untouched.
		expect(frame.channels[0][0]).toBe(1);
	});

	test("reaches silence after the full fade", () => {
		const gain = new Gain();
		gain.set(0);

		// 0.2s of audio at 48kHz, in realistic 128-sample quanta.
		let last = 1;
		for (let n = 0; n < Math.ceil(0.2 * RATE) / 128; n++) {
			const frame = ones(128);
			last = gain.apply(frame, RATE).channels[0][127];
		}

		expect(last).toBeCloseTo(0, 5);
	});

	test("ramps every channel in step", () => {
		const gain = new Gain();

		// Distinct amplitudes: two channels of the same value would compare equal even if one were
		// overwritten with its neighbour rather than scaled in place.
		const frame: AudioFrame = {
			timestamp: 0 as Time.Micro,
			channels: [new Float32Array(128).fill(1), new Float32Array(128).fill(-0.5)],
		};

		gain.set(0);
		const out = gain.apply(frame, RATE);

		// Every channel rides one level, so each keeps its own value and their ratio is untouched.
		for (let index = 0; index < 128; index++) {
			expect(out.channels[1][index]).toBeCloseTo(out.channels[0][index] * -0.5, 6);
		}

		// And the level actually moved, so the ratio above isn't just 0 === 0.
		expect(out.channels[0][127]).toBeLessThan(1);
		expect(out.channels[0][127]).toBeGreaterThan(0);
	});

	// The ramp is per-sample, so a slower rate has to cover the same fade in fewer samples.
	test("scales the ramp to the sample rate", () => {
		const fast = new Gain();
		const slow = new Gain();

		const fastFrame = ones(128);
		const slowFrame = ones(128);

		fast.set(0);
		slow.set(0);
		const fastOut = fast.apply(fastFrame, RATE);
		const slowOut = slow.apply(slowFrame, SLOW_RATE);

		expect(slowOut.channels[0][127]).toBeLessThan(fastOut.channels[0][127]);
	});
});
