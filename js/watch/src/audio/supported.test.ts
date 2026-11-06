import { expect, mock, spyOn, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Source } from "./source";

// Bun cannot load the blob-URL worklet import, so mock it before the decoder imports it.
mock.module("./render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));
const { Decoder } = await import("./decoder");

test("renditions are probed through the libav fallback without a native AudioDecoder", async () => {
	// The premise: Bun ships no WebCodecs, so only the fallback can report support.
	expect(globalThis.AudioDecoder).toBeUndefined();

	const config = (fields: Record<string, unknown>) =>
		Catalog.AudioConfigSchema.parse({
			container: { kind: "legacy" },
			sampleRate: 48000,
			numberOfChannels: 2,
			...fields,
		});

	// 0x1190 is the AudioSpecificConfig for AAC-LC, 48kHz, stereo.
	expect(await Decoder.supported(config({ codec: "mp4a.40.2", description: "1190" }))).toBe(true);
	expect(await Decoder.supported(config({ codec: "opus" }))).toBe(true);
	expect(await Decoder.supported(config({ codec: "flac" }))).toBe(false);
});

test("the decoder support probe is reused through mute and unmute", async () => {
	const config = Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48000,
		numberOfChannels: 2,
	});
	// Load the real libav decoder before counting its WebCodecs support queries.
	await Decoder.supported(config);
	using queries = spyOn(AudioDecoder, "isConfigSupported");
	const catalog = new Signal<Catalog.Root>({ audio: { renditions: { audio: config } } });
	const source = new Source({
		broadcast: { out: { catalog } } as unknown as Broadcast,
		supported: Decoder.supported,
	});
	try {
		await source.out.available.changed();
		for (const enabled of [false, true]) {
			const changed = source.out.available.changed();
			catalog.set({ audio: { renditions: { audio: { ...config, enabled } } } });
			await changed;
			expect(source.out.available.peek().audio.enabled).toBe(enabled);
		}
		expect(queries).toHaveBeenCalledTimes(1);
	} finally {
		source.close();
	}
});
