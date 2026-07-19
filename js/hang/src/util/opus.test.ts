import { describe, expect, it } from "bun:test";
import { normalizeSampleRate } from "./opus";

describe("normalizeSampleRate", () => {
	it("defaults to fullband Opus", () => {
		expect(normalizeSampleRate()).toBe(48_000);
	});

	it("preserves native Opus rates", () => {
		for (const sampleRate of [8_000, 12_000, 16_000, 24_000, 48_000]) {
			expect(normalizeSampleRate(sampleRate)).toBe(sampleRate);
		}
	});

	it("rounds up to preserve the input bandwidth", () => {
		expect(normalizeSampleRate(11_025)).toBe(12_000);
		expect(normalizeSampleRate(22_050)).toBe(24_000);
		expect(normalizeSampleRate(32_000)).toBe(48_000);
		expect(normalizeSampleRate(44_100)).toBe(48_000);
	});

	it("caps rates above fullband Opus", () => {
		expect(normalizeSampleRate(96_000)).toBe(48_000);
	});

	it("rejects invalid rates", () => {
		expect(() => normalizeSampleRate(0)).toThrow("invalid Opus sample rate");
		expect(() => normalizeSampleRate(Number.NaN)).toThrow("invalid Opus sample rate");
	});
});
