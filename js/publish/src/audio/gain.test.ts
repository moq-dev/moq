import { describe, expect, test } from "bun:test";
import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Gain } from "./gain";

const RATE = 48_000;
const FADE = Time.Milli(50);
// The samples a FADE ramp spans at RATE.
const FADE_SAMPLES = (FADE / 1000) * RATE;

// Half the rate covers the same fade in half the samples, so the ramp has to move twice as fast.
const SLOW_RATE = RATE / 2;

function ones(samples: number, channels = 1): AudioFrame {
	return {
		timestamp: 0 as Time.Micro,
		channels: Array.from({ length: channels }, () => new Float32Array(samples).fill(1)),
	};
}

describe("Gain", () => {
	test("starts at the requested level", () => {
		const muted = new Gain(0);
		const quiet = new Gain(0.25);

		expect([...muted.apply(ones(128), RATE).channels[0]]).toEqual(Array.from({ length: 128 }, () => 0));
		expect([...quiet.apply(ones(128), RATE).channels[0]]).toEqual(Array.from({ length: 128 }, () => 0.25));
	});

	test("leaves audio untouched at unity", () => {
		const gain = new Gain();
		const frame = ones(128);

		gain.set(1, FADE);
		const out = gain.apply(frame, RATE);

		expect([...out.channels[0]]).toEqual(Array.from({ length: 128 }, () => 1));
	});

	// Jumping straight to the target is what clicks, so the level has to walk there.
	test("ramps toward a mute rather than jumping", () => {
		const gain = new Gain();
		const frame = ones(128);

		gain.set(0, FADE);
		const out = gain.apply(frame, RATE);

		// 128 samples is far less than the 50ms fade, so it has barely moved.
		expect(out.channels[0][0]).toBeLessThan(1);
		expect(out.channels[0][0]).toBeGreaterThan(0.99);
		expect(out.channels[0][127]).toBeLessThan(out.channels[0][0]);
		expect(out.channels[0][127]).toBeGreaterThan(0.94);

		// The input is shared with every other rendition, so it must come back untouched.
		expect(frame.channels[0][0]).toBe(1);
	});

	// A mute is what keeps the microphone private, so it has to be silent on time, not merely quiet.
	test("is silent once the fade passes", () => {
		const gain = new Gain();
		gain.set(0, FADE);

		// Realistic 128-sample quanta, which don't divide the fade evenly.
		const out: number[] = [];
		while (out.length < FADE_SAMPLES + 256) out.push(...gain.apply(ones(128), RATE).channels[0]);

		expect(out[FADE_SAMPLES - 2]).toBeGreaterThan(0);
		expect(out.slice(FADE_SAMPLES - 1).every((sample) => sample === 0)).toBe(true);
	});

	// A ramp paced per unit of level would finish a small change early and a large one late.
	test("takes the whole fade whatever the size of the change", () => {
		const gain = new Gain(0.1);
		gain.set(0, FADE);

		const out = gain.apply(ones(FADE_SAMPLES), RATE).channels[0];
		expect(out[FADE_SAMPLES / 2 - 1]).toBeCloseTo(0.05, 6);
		expect(out[FADE_SAMPLES - 2]).toBeGreaterThan(0);
		expect(out[FADE_SAMPLES - 1]).toBe(0);
	});

	test("steps at once with no fade", () => {
		const gain = new Gain();
		gain.set(0, Time.Milli(0));

		expect([...gain.apply(ones(128), RATE).channels[0]]).toEqual(Array.from({ length: 128 }, () => 0));
	});

	// The encoder sets the volume on every frame, which must not restart the ramp it is already on.
	test("repeating the target keeps the ramp going", () => {
		const gain = new Gain();
		gain.set(0, FADE);
		gain.apply(ones(FADE_SAMPLES / 2), RATE);

		gain.set(0, FADE);
		const out = gain.apply(ones(FADE_SAMPLES / 2), RATE).channels[0];
		expect(out[FADE_SAMPLES / 2 - 1]).toBe(0);
	});

	test("ramps every channel in step", () => {
		const gain = new Gain();

		// Distinct amplitudes: two channels of the same value would compare equal even if one were
		// overwritten with its neighbour rather than scaled in place.
		const frame: AudioFrame = {
			timestamp: 0 as Time.Micro,
			channels: [new Float32Array(128).fill(1), new Float32Array(128).fill(-0.5)],
		};

		gain.set(0, FADE);
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

		fast.set(0, FADE);
		slow.set(0, FADE);
		const fastOut = fast.apply(fastFrame, RATE);
		const slowOut = slow.apply(slowFrame, SLOW_RATE);

		expect(slowOut.channels[0][127]).toBeLessThan(fastOut.channels[0][127]);
	});
});
