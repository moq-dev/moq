import { describe, expect, it } from "bun:test";
import { Duration, Instant } from "@moq/clock";
import { Peak, ringCapacity, ringHold, ringSamples, target } from "./latency";

const ms = (value: number) => Duration.millis(value);

describe("target", () => {
	it("adds one frame to the measured term", () => {
		expect(target({ measured: ms(40), frame: ms(20) })).toEqual(ms(60));
	});

	it("adds the output latency, since audio is placed where it is heard", () => {
		expect(target({ measured: ms(40), frame: ms(20), output: ms(42) })).toEqual(ms(102));
	});

	it("floors the measured term at the advertised span instead of adding it", () => {
		expect(target({ measured: ms(40), advertised: ms(200), frame: ms(20) })).toEqual(ms(220));
		expect(target({ measured: ms(300), advertised: ms(200), frame: ms(20) })).toEqual(ms(320));
	});

	it("adds the catalog delay on top, since the measurement cancels a cross-track offset", () => {
		expect(target({ measured: ms(40), advertised: ms(60), frame: ms(20), delay: ms(200) })).toEqual(ms(280));
	});

	it("is the measured term alone when nothing else is known", () => {
		expect(target({ measured: ms(100) })).toEqual(ms(100));
	});
});

describe("ringSamples", () => {
	// `delay="instant"` reports a zero buffer. Passed through, the ring rejects it and the
	// worklet is left with no backend that any later update can revive.
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

	// A catalog's jitter or delay is any u53, so the part held decoded is the receiver's to cap.
	it("caps the decoded part at half the fixed capacity", () => {
		expect(ringSamples(48_000, ms(60 * 60 * 1000))).toBe(24_000);
		expect(ringCapacity(48_000)).toBe(48_000);
	});
});

describe("ringHold", () => {
	it("holds the target above the cap", () => {
		expect(ringHold(ms(1_500))).toEqual(Duration.micros(1_000_000));
	});

	// At 44.1 kHz this target's floor rounds to a sample count a few picoseconds short of it.
	it("holds exactly nothing for a target within the cap", () => {
		expect(ringHold(ms(203.21995464852608))).toEqual(Duration.micros(0));
		expect(ringHold(ms(500))).toEqual(Duration.micros(0));
	});
});

describe("Peak", () => {
	// A lower reading holds until the higher one ages out of the window; a higher one takes over at once.
	it("holds the highest output latency for five seconds", () => {
		const at = (secs: number) => Instant.ZERO.saturatingAdd(Duration.secs(secs));
		const peak = new Peak();
		expect(peak.observe(ms(120), at(0))).toEqual(ms(120));
		expect(peak.observe(ms(104), at(1))).toEqual(ms(120));
		expect(peak.observe(ms(112), at(5))).toEqual(ms(120));
		expect(peak.observe(ms(108), at(6))).toEqual(ms(112));
		expect(peak.observe(ms(128), at(7))).toEqual(ms(128));
	});
});
