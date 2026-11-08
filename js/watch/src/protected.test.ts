import { expect, mock, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";

// Bun cannot load the blob-URL worklet import, so mock it before the decoder imports it.
mock.module("./audio/render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));
const Audio = await import("./audio/decoder");
const Video = await import("./video/decoder");

// WebCodecs cannot decrypt, so `<moq-watch>` treats a protected rendition as unplayable before it
// asks the decoder, whatever the codec.
test("protected renditions are unplayable", async () => {
	const cmaf = { kind: "cmaf", init: "AAAA" };
	const video = Catalog.VideoConfigSchema.parse({ codec: "avc1.64001f", container: cmaf, drm: ["1"] });
	const audio = Catalog.AudioConfigSchema.parse({
		codec: "mp4a.40.2",
		container: cmaf,
		sampleRate: 48000,
		numberOfChannels: 2,
		drm: ["1"],
	});
	expect(await Video.Decoder.supported(video)).toBe(false);
	expect(await Audio.Decoder.supported(audio)).toBe(false);
});

// An encrypted sample entry marks a CMAF rendition protected even when its catalog lists no `drm`
// entry, so a publisher that encrypts without signalling it is never decoded as clear.
test("an encrypted CMAF init is unplayable without drm entries", async () => {
	const fixtures = JSON.parse(await Bun.file(new URL("../../hang/src/cmaf/fixtures.json", import.meta.url)).text());
	const encrypt = (name: string, clear: string, encrypted: string) => {
		const init = Buffer.from(fixtures[name], "base64");
		init.write(encrypted, init.indexOf(clear, init.indexOf("moov")), "latin1");
		return init.toString("base64");
	};
	const video = Catalog.VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "cmaf", init: encrypt("videoInit", "avc1", "encv") },
	});
	const audio = Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "cmaf", init: encrypt("audioInit", "Opus", "enca") },
		sampleRate: 48000,
		numberOfChannels: 2,
	});
	expect(await Video.Decoder.supported(video)).toBe(false);
	expect(await Audio.Decoder.supported(audio)).toBe(false);
});
