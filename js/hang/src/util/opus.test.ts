import { describe, expect, it } from "bun:test";
import { pickRate, SAMPLE_RATES, supportsRate, toDOps } from "./opus";

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

describe("toDOps", () => {
	it("converts OpusHead fields and byte order", () => {
		const head = new Uint8Array(19);
		head.set(new TextEncoder().encode("OpusHead"));
		head[8] = 1;
		head[9] = 2;
		const view = new DataView(head.buffer);
		view.setUint16(10, 312, true);
		view.setUint32(12, 48_000, true);
		view.setInt16(16, -2, true);

		const dops = toDOps(head);
		const dopsView = new DataView(dops.buffer);
		expect(dops[0]).toBe(0);
		expect(dops[1]).toBe(2);
		expect(dopsView.getUint16(2, false)).toBe(312);
		expect(dopsView.getUint32(4, false)).toBe(48_000);
		expect(dopsView.getInt16(8, false)).toBe(-2);
		expect(dops[10]).toBe(0);
	});

	it("passes an existing dOps payload through", () => {
		const dops = Uint8Array.from([0, 2, 1, 56, 0, 0, 187, 128, 0, 0, 0]);
		expect(toDOps(dops)).toEqual(dops);
	});
});
