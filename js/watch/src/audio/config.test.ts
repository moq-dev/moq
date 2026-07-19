import { describe, expect, it } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { audioDecoderConfig, audioDecoderSampleRate } from "./config";

function config(codec: string, sampleRate: number): Catalog.AudioConfig {
	return {
		codec,
		sampleRate: Catalog.u53(sampleRate),
		numberOfChannels: Catalog.u53(2),
		container: { kind: "legacy" },
	};
}

describe("audioDecoderSampleRate", () => {
	it("normalizes a legacy 44.1kHz Opus catalog to fullband output", () => {
		expect(audioDecoderSampleRate(config("opus", 44_100))).toBe(48_000);
	});

	it("preserves native Opus output rates", () => {
		expect(audioDecoderSampleRate(config("opus", 16_000))).toBe(16_000);
	});

	it("does not normalize other codecs", () => {
		expect(audioDecoderSampleRate(config("mp4a.40.2", 44_100))).toBe(44_100);
	});
});

it("builds the decoder config with the normalized output rate", () => {
	const description = new Uint8Array([1, 2, 3]);
	expect(audioDecoderConfig(config("opus", 44_100), description)).toEqual({
		codec: "opus",
		sampleRate: 48_000,
		numberOfChannels: 2,
		description,
	});
});
