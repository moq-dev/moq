import { describe, expect, it } from "bun:test";
import { audioSpecificConfig } from "./aac";

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

	it("falls back to the 44.1kHz index for an unknown rate", () => {
		expect(audioSpecificConfig(12345, 2)).toEqual(audioSpecificConfig(44100, 2));
	});
});
