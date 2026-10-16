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

test("pcm codec is accepted", () => {
	const config = AudioConfigSchema.parse({
		codec: "pcm",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
		bitrate: 3_072_000,
	});

	expect(config.codec).toBe("pcm");
});

test("audio config accepts a human-readable label", () => {
	const config = AudioConfigSchema.parse({
		label: "English",
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
	});

	expect(config.label).toBe("English");
});

test("audio config accepts optional enabled state", () => {
	const config = AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48_000,
		numberOfChannels: 2,
		enabled: false,
	});

	expect(config.enabled).toBe(false);
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
