import { describe, expect, it } from "bun:test";
import { pickRate, SAMPLE_RATES, supportsRate } from "./opus";

describe("pickRate", () => {
	// Matches the pick_opus_rate tests in rs/moq-audio/src/codec.rs.
	it("snaps 44.1kHz up to 48kHz", () => {
		expect(pickRate(44100)).toBe(48000);
	});

	it("snaps 22.05kHz up to 24kHz", () => {
		expect(pickRate(22050)).toBe(24000);
	});

	it("leaves the native rates alone", () => {
		for (const rate of SAMPLE_RATES) {
			expect(pickRate(rate)).toBe(rate);
		}
	});

	it("falls back to 48kHz above the highest rate", () => {
		expect(pickRate(96000)).toBe(48000);
	});

	it("snaps up to the lowest rate", () => {
		expect(pickRate(1)).toBe(8000);
	});
});

describe("supportsRate", () => {
	it("rejects rates Opus cannot run at", () => {
		// The rate a Bluetooth headset on macOS reports after an A2DP flip, and the one Safari's
		// AudioDecoder accepts via isConfigSupported and then fails on.
		expect(supportsRate(44100)).toBe(false);
		expect(supportsRate(22050)).toBe(false);
	});

	it("accepts the native rates", () => {
		for (const rate of SAMPLE_RATES) {
			expect(supportsRate(rate)).toBe(true);
		}
	});
});
