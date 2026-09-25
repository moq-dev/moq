import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { reanchor, ringSamples, target } from "./latency";

const ms = (value: number) => value as Time.Milli;

describe("target", () => {
	it("adds one frame to the measured term", () => {
		expect(target({ measured: ms(40), frame: ms(20) })).toBe(ms(60));
	});

	it("floors the measured term at the advertised span instead of adding it", () => {
		expect(target({ measured: ms(40), advertised: ms(200), frame: ms(20) })).toBe(ms(220));
		expect(target({ measured: ms(300), advertised: ms(200), frame: ms(20) })).toBe(ms(320));
	});

	it("is the measured term alone when nothing else is known", () => {
		expect(target({ measured: ms(100) })).toBe(ms(100));
	});
});

describe("ringSamples", () => {
	// `delay="instant"` reports a zero buffer. Passed through, the ring rejects it and the
	// worklet is left with no backend that any later resize can revive.
	it("floors a zero delay at one render quantum", () => {
		expect(ringSamples(48_000, ms(0))).toBe(128);
	});

	it("floors a delay too short to fill a quantum", () => {
		// 1ms at 48kHz is 48 samples.
		expect(ringSamples(48_000, ms(1))).toBe(128);
	});

	it("leaves a delay above the floor alone", () => {
		expect(ringSamples(48_000, ms(100))).toBe(4_800);
	});
});

describe("reanchor", () => {
	it("re-stalls once a run of one-frame rises adds up to more than a frame", () => {
		let baseline = ms(40);
		const stalls = [60, 80, 100].map((next) => {
			const result = reanchor(baseline, ms(next), ms(20));
			baseline = result.baseline;
			return result.stall;
		});
		expect(stalls).toEqual([false, true, false]);
		expect(baseline).toBe(ms(80));
	});

	it("follows a fall down, so the next rise is measured from there", () => {
		expect(reanchor(ms(100), ms(60), ms(20))).toEqual({ stall: false, baseline: ms(60) });
		expect(reanchor(ms(60), ms(100), ms(20))).toEqual({ stall: true, baseline: ms(100) });
	});
});
