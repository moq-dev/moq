import { describe, expect, mock, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import type { Kind } from "./types.ts";

// The ?worklet suffix is a Vite plugin transform; stub it so bun can import encoder.ts. The dynamic
// import below has to run after this, since a static import of encoder.ts would be hoisted above it.
mock.module("./capture-worklet.ts?worklet", () => ({ default: "" }));
const { toEncoderConfig } = await import("./encoder.ts");

const OPUS: Catalog.AudioConfig = {
	codec: "opus",
	container: { kind: "legacy" },
	sampleRate: Catalog.u53(48_000),
	numberOfChannels: Catalog.u53(2),
	bitrate: Catalog.u53(64_000),
};

// The Opus-only knobs the encoder eventually hands to WebCodecs.
function opus(kind: Kind, opusOptions: Record<string, unknown> = {}): Record<string, unknown> | undefined {
	return toEncoderConfig(OPUS, kind, opusOptions).opus as Record<string, unknown> | undefined;
}

describe("toEncoderConfig opus kind defaults", () => {
	test("voice enables DTX with voip/voice tuning", () => {
		const o = opus("voice");
		expect(o?.application).toBe("voip");
		expect(o?.signal).toBe("voice");
		expect(o?.usedtx).toBe(true);
	});

	test("music uses audio/music tuning and leaves DTX to the browser", () => {
		const o = opus("music");
		expect(o?.application).toBe("audio");
		expect(o?.signal).toBe("music");
		expect(o?.usedtx).toBeUndefined();
	});

	test("auto sets no kind-derived opus knobs", () => {
		const o = opus("auto");
		expect(o?.application).toBeUndefined();
		expect(o?.signal).toBeUndefined();
		expect(o?.usedtx).toBeUndefined();
	});

	test("an explicit usedtx overrides the kind default", () => {
		expect(opus("voice", { usedtx: false })?.usedtx).toBe(false);
		expect(opus("music", { usedtx: true })?.usedtx).toBe(true);
	});
});
