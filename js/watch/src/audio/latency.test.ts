import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { reanchorFloor, ringSamples } from "./latency";

const ms = (value: number) => value as Time.Milli;

describe("reanchorFloor", () => {
	it("includes the fixed delay and the largest media delay", () => {
		expect(reanchorFloor({ delay: ms(100), audio: ms(20), video: ms(80) })).toBe(ms(180));
	});

	it("tracks rendition delay without adaptive RTT jitter", () => {
		expect(reanchorFloor({ delay: "auto", audio: ms(20), video: ms(80) })).toBe(ms(80));
		expect(reanchorFloor({ delay: "auto", audio: ms(20), video: ms(200) })).toBe(ms(200));
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
