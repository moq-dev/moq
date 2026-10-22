import { expect, test } from "bun:test";
import { AudioConfigSchema } from "./audio.ts";
import * as Catalog from "./index.ts";

test("audio ranks by enabled, bitrate, sample rate, channels, and name", () => {
	const rendition = (sampleRate: number, numberOfChannels: number, bitrate?: number) =>
		AudioConfigSchema.parse({
			codec: "opus",
			container: { kind: "legacy" },
			sampleRate,
			numberOfChannels,
			bitrate,
		});
	const renditions = {
		e: rendition(48_000, 2, 64_000),
		a: rendition(48_000, 2),
		g: rendition(16_000, 1),
		c: rendition(16_000, 1, 128_000),
		f: rendition(44_100, 2, 64_000),
		b: rendition(48_000, 1, 64_000),
		d: rendition(48_000, 2, 64_000),
	};
	expect(Catalog.Audio.ranked(renditions).map(([name]) => name)).toEqual(["c", "d", "e", "b", "f", "a", "g"]);
	renditions.c.enabled = false;
	expect(Catalog.Audio.ranked(renditions).map(([name]) => name)).toEqual(["d", "e", "b", "f", "a", "g", "c"]);
});

test("audio ranks known zero bitrate above unknown and breaks ties by code point", () => {
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});
	const zero = AudioConfigSchema.parse({ ...config, bitrate: 0 });
	const renditions = { unknown: config, "\u{1F600}": zero, "\uE000": zero };
	expect(Catalog.Audio.ranked(renditions).map(([name]) => name)).toEqual(["\uE000", "\u{1F600}", "unknown"]);
});

test("audio warmup round trips as optional integer milliseconds", () => {
	const base = { codec: "opus", container: { kind: "legacy" }, sampleRate: 48_000, numberOfChannels: 2 };
	expect(AudioConfigSchema.parse(base).warmup).toBeUndefined();
	for (const warmup of [0, 80, 1_000]) {
		const config = AudioConfigSchema.parse({ ...base, warmup });
		expect(Number(config.warmup)).toBe(warmup);
		expect(AudioConfigSchema.parse(JSON.parse(JSON.stringify(config)))).toEqual(config);
	}
	for (const warmup of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
		expect(() => AudioConfigSchema.parse({ ...base, warmup })).toThrow();
	}
});

test("ambisonic layout needs a square channel count of at least 4", () => {
	const base = { codec: "opus", container: { kind: "legacy" }, sampleRate: 48_000, layout: "ambisonic" };
	for (const numberOfChannels of [4, 9, 16, 225]) {
		expect(AudioConfigSchema.parse({ ...base, numberOfChannels }).layout).toBe("ambisonic");
	}
	for (const numberOfChannels of [0, 1, 2, 6, 8]) {
		expect(() => AudioConfigSchema.parse({ ...base, numberOfChannels })).toThrow();
	}
});

test("audio layout is optional and keeps unknown values verbatim", () => {
	const base = { codec: "opus", container: { kind: "legacy" }, sampleRate: 48_000, numberOfChannels: 6 };
	expect(AudioConfigSchema.parse(base).layout).toBeUndefined();
	// An unknown layout puts no constraint on the channel count.
	expect(AudioConfigSchema.parse({ ...base, layout: "surround" }).layout).toBe("surround");
	expect(() => AudioConfigSchema.parse({ ...base, layout: 1 })).toThrow();
});
