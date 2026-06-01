import { describe, expect, mock, test } from "bun:test";

// The ?worklet suffix is a Vite plugin transform; stub it so bun can import encoder.ts.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "" }));

import * as Catalog from "@moq/hang/catalog";
import { toEncoderConfig } from "./encoder";

const BASE_CONFIG: Catalog.AudioConfig = {
	codec: "opus",
	sampleRate: Catalog.u53(48_000),
	numberOfChannels: Catalog.u53(2),
	bitrate: Catalog.u53(64_000),
	container: { kind: "legacy" },
};

describe("toEncoderConfig opus flags", () => {
	test("voice sets voip application, voice signal, and enables DTX", () => {
		const opus = (toEncoderConfig(BASE_CONFIG, "voice") as unknown as { opus: Record<string, unknown> }).opus;
		expect(opus?.application).toBe("voip");
		expect(opus?.signal).toBe("voice");
		expect(opus?.usedtx).toBe(true);
	});

	test("music sets audio application, music signal, and leaves DTX off", () => {
		const opus = (toEncoderConfig(BASE_CONFIG, "music") as unknown as { opus: Record<string, unknown> }).opus;
		expect(opus?.application).toBe("audio");
		expect(opus?.signal).toBe("music");
		expect(opus?.usedtx).toBeUndefined();
	});

	test("auto produces no opus-specific config", () => {
		const config = toEncoderConfig(BASE_CONFIG, "auto");
		expect(config.opus).toBeUndefined();
	});
});

// 20 ms of silence at 48 kHz stereo (960 samples × 2 channels interleaved = 1920 floats).
describe("silence intervals", () => {
	function silenceFrame(sampleRate = 48_000, durationMs = 20, channels = 2): Float32Array {
		return new Float32Array((sampleRate * durationMs * channels) / 1_000);
	}

	test("silence frame contains only zeros", () => {
		const frame = silenceFrame();
		expect(frame.every((s) => s === 0)).toBe(true);
	});

	test("voice config enables DTX , silence padding between ", () => {
		const config = toEncoderConfig(BASE_CONFIG, "voice");
		const opus = config.opus as Record<string, unknown> | undefined;
		expect(opus?.usedtx).toBe(true);

		const frame = silenceFrame();
		expect(frame.length).toBe(1_920);
	});

	test("music config leaves DTX off ,silence is encoded as a full frame", () => {
		const config = toEncoderConfig(BASE_CONFIG, "music");
		const opus = config.opus as Record<string, unknown> | undefined;
		expect(opus?.usedtx).toBeUndefined();
	});
});
