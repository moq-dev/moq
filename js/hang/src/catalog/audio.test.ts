import { expect, test } from "bun:test";
import { AudioConfigSchema } from "./audio.ts";

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
