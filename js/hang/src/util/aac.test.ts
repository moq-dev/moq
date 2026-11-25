import { describe, expect, it } from "bun:test";
import { audioSpecificConfig, objectType, parseConfig, pickRate, SAMPLE_RATES, supportsRate } from "./aac";

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

	// A count with no channel configuration is refused rather than described as stereo.
	it("refuses a channel count with no configuration", () => {
		for (const count of [0, 7, 9]) {
			expect(() => audioSpecificConfig(48000, count)).toThrow();
		}
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

describe("objectType", () => {
	// Every spelling the WebCodecs AAC registration lists.
	it("classifies every registered alias", () => {
		expect(objectType("mp4a.40.2")).toBe(2);
		expect(objectType("mp4a.40.02")).toBe(2);
		expect(objectType("mp4a.67")).toBe(2);
		expect(objectType("mp4a.40.5")).toBe(5);
		expect(objectType("mp4a.40.05")).toBe(5);
		expect(objectType("mp4a.40.29")).toBe(29);
	});

	it("rejects anything the registration does not name", () => {
		// AAC Main, MP3-in-MP4, xHE-AAC, and strings that only resemble a registered spelling.
		const codecs = ["mp4a.40.1", "mp4a.40.34", "mp4a.40.42", "mp4a.40.0005", "mp4a.40.5 ", "mp4a.6B", "opus"];
		for (const codec of codecs) {
			expect(objectType(codec)).toBeUndefined();
		}
	});
});

describe("parseConfig", () => {
	it("reads AAC-LC", () => {
		expect(parseConfig(new Uint8Array([0x12, 0x10]))).toEqual({ sampleRate: 44100, numberOfChannels: 2 });
	});

	it("reads the escape form audioSpecificConfig writes", () => {
		expect(parseConfig(audioSpecificConfig(44101, 8))).toEqual({ sampleRate: 44101, numberOfChannels: 8 });
	});

	// A 24 kHz core under explicit SBR or PS plays at the 48 kHz extension rate, PS in stereo.
	it("reports explicit SBR and PS output", () => {
		expect(parseConfig(new Uint8Array([0x2b, 0x11, 0x88]))).toEqual({ sampleRate: 48000, numberOfChannels: 2 });
		expect(parseConfig(new Uint8Array([0xeb, 0x09, 0x88]))).toEqual({ sampleRate: 48000, numberOfChannels: 2 });
	});

	// An AAC-LC core followed by a backward-compatible sync extension plays at the signaled output.
	it("reports sync extension SBR and PS output", () => {
		// A 24 kHz stereo core with SBR at 48 kHz.
		const sbr = new Uint8Array([0x13, 0x10, 0x56, 0xe5, 0x98]);
		expect(parseConfig(sbr)).toEqual({ sampleRate: 48000, numberOfChannels: 2 });
		// A 24 kHz mono core with SBR at 48 kHz and PS.
		const ps = new Uint8Array([0x13, 0x08, 0x56, 0xe5, 0x9d, 0x48, 0x80]);
		expect(parseConfig(ps)).toEqual({ sampleRate: 48000, numberOfChannels: 2 });
		// sbrPresentFlag clear, or the extension cut short, leaves the core.
		const absent = new Uint8Array([0x13, 0x10, 0x56, 0xe5, 0x00]);
		expect(parseConfig(absent)).toEqual({ sampleRate: 24000, numberOfChannels: 2 });
		expect(parseConfig(sbr.subarray(0, 4))).toEqual({ sampleRate: 24000, numberOfChannels: 2 });
	});

	it("refuses what it can't read", () => {
		expect(parseConfig(new Uint8Array([0x12]))).toBeUndefined();
		// A program config element's channels, and a reserved channelConfiguration.
		expect(parseConfig(new Uint8Array([0x12, 0x00]))).toBeUndefined();
		expect(parseConfig(new Uint8Array([0x12, 0x40]))).toBeUndefined();
	});
});
