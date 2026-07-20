import { describe, expect, it } from "bun:test";
import { audioSpecificConfig, pickRate, SAMPLE_RATES, supportsRate } from "./aac";

describe("pickRate", () => {
	// 44.1kHz is in the AAC table, unlike Opus, so it must survive untouched.
	it("leaves the table rates alone", () => {
		for (const rate of SAMPLE_RATES) {
			expect(pickRate(rate)).toBe(rate);
		}
	});

	it("snaps an off-table rate up to the next one", () => {
		expect(pickRate(44101)).toBe(48000);
		expect(pickRate(20000)).toBe(22050);
	});

	it("falls back to the highest rate above the table", () => {
		expect(pickRate(192000)).toBe(96000);
	});
});

describe("supportsRate", () => {
	it("accepts 44.1kHz", () => {
		expect(supportsRate(44100)).toBe(true);
	});

	it("rejects an off-table rate", () => {
		expect(supportsRate(44101)).toBe(false);
	});
});

describe("audioSpecificConfig", () => {
	// Well-known AAC-LC AudioSpecificConfig values.
	it("48kHz stereo", () => {
		expect(audioSpecificConfig(48000, 2)).toEqual(new Uint8Array([0x11, 0x90]));
	});

	it("44.1kHz stereo", () => {
		expect(audioSpecificConfig(44100, 2)).toEqual(new Uint8Array([0x12, 0x10]));
	});

	it("44.1kHz mono", () => {
		expect(audioSpecificConfig(44100, 1)).toEqual(new Uint8Array([0x12, 0x08]));
	});

	// 8 channels maps to channelConfiguration 7 (7.1), not a raw 8.
	it("48kHz 7.1 maps to channel config 7", () => {
		expect(audioSpecificConfig(48000, 8)).toEqual(new Uint8Array([0x11, 0xb8]));
	});

	// Unsupported channel counts fall back to stereo (config 2).
	it("unsupported channel count falls back to stereo", () => {
		expect(audioSpecificConfig(48000, 7)).toEqual(audioSpecificConfig(48000, 2));
	});

	// Non-table sample rates use the 5-byte explicit-frequency form (freqIndex 0xF).
	it("non-standard rate uses the explicit-frequency form", () => {
		const asc = audioSpecificConfig(64001, 2);
		expect(asc.length).toBe(5);
		// 5 bits AOT(2) + 4 bits 0xF: 00010 1111 ... -> 0x17, then bit7 = freq escape low bit.
		expect(asc[0]).toBe(0x17);
		// Round-trip the 24-bit sample rate packed at bits 30..7 of the trailing 4 bytes.
		const tail = (asc[1] << 24) | (asc[2] << 16) | (asc[3] << 8) | asc[4];
		expect((tail >>> 7) & 0xffffff).toBe(64001);
	});
});
