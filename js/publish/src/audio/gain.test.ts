import { describe, expect, test } from "bun:test";
import type { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Gain } from "./gain";

const RATE = 48_000;

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
		gain.apply(frame, RATE);

		expect([...frame.channels[0]]).toEqual(Array.from({ length: 128 }, () => 1));
	});

	// Jumping straight to the target is what clicks, so the level has to walk there.
	test("ramps toward a mute rather than jumping", () => {
		const gain = new Gain();
		const frame = ones(128);

		gain.set(0);
		gain.apply(frame, RATE);

		// 128 samples is far less than the 0.2s fade, so it has barely moved.
		expect(frame.channels[0][0]).toBeLessThan(1);
		expect(frame.channels[0][0]).toBeGreaterThan(0.99);
		expect(frame.channels[0][127]).toBeLessThan(frame.channels[0][0]);
		expect(frame.channels[0][127]).toBeGreaterThan(0.98);
	});

	test("reaches silence after the full fade", () => {
		const gain = new Gain();
		gain.set(0);

		// 0.2s of audio at 48kHz, in realistic 128-sample quanta.
		let last = 1;
		for (let n = 0; n < Math.ceil(0.2 * RATE) / 128; n++) {
			const frame = ones(128);
			gain.apply(frame, RATE);
			last = frame.channels[0][127];
		}

		expect(last).toBeCloseTo(0, 5);
	});

	test("ramps every channel in step", () => {
		const gain = new Gain();
		const frame = ones(128, 2);

		gain.set(0);
		gain.apply(frame, RATE);

		expect([...frame.channels[0]]).toEqual([...frame.channels[1]]);
	});

	// The ramp is per-sample, so a slower rate has to cover the same fade in fewer samples.
	test("scales the ramp to the sample rate", () => {
		const fast = new Gain();
		const slow = new Gain();

		const fastFrame = ones(128);
		const slowFrame = ones(128);

		fast.set(0);
		slow.set(0);
		fast.apply(fastFrame, 48_000);
		slow.apply(slowFrame, 24_000);

		expect(slowFrame.channels[0][127]).toBeLessThan(fastFrame.channels[0][127]);
	});
});
