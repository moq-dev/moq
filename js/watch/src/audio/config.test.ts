import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { decoderConfig, frameDuration, packetDuration, playbackIdentity } from "./config";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function config(fields: Record<string, unknown> = {}): Catalog.AudioConfig {
	return Catalog.AudioConfigSchema.parse({
		codec: "opus",
		container: { kind: "legacy" },
		sampleRate: 48000,
		numberOfChannels: 2,
		...fields,
	});
}

test("metadata and routing changes do not change the decoder config", async () => {
	const rendition = new Signal<Catalog.AudioConfig>(config());
	const root = new Effect();
	const decoder = root.computed((effect) => decoderConfig(effect.get(rendition)));
	let worklets = 0;

	root.run((effect) => {
		effect.get(decoder);
		worklets++;
	});
	await flush();
	expect(worklets).toBe(1);

	// A bitrate-only republish, the shape the MPEG-TS importer emits as it refines its estimate.
	rendition.set(config({ bitrate: 128_000 }));
	await flush();
	expect(worklets).toBe(1);

	rendition.set(config({ bitrate: 96_000, jitter: 20, timeline: { track: "timeline" } }));
	await flush();
	expect(worklets).toBe(1);

	rendition.set(config({ broadcast: "../source" }));
	await flush();
	expect(worklets).toBe(1);

	rendition.set(config({ sampleRate: 44100 }));
	await flush();
	expect(worklets).toBe(2);

	root.close();
});

test("routing and decoder inputs change the playback identity", () => {
	const base = playbackIdentity(config());

	expect(playbackIdentity(config({ broadcast: "../source" }))).not.toEqual(base);
	expect(playbackIdentity(config({ codec: "mp4a.40.2" }))).not.toEqual(base);
	expect(playbackIdentity(config({ container: { kind: "loc" } }))).not.toEqual(base);
	expect(playbackIdentity(config({ description: "4f707573486561640102" }))).not.toEqual(base);
	expect(playbackIdentity(config({ sampleRate: 44100 }))).not.toEqual(base);
	expect(playbackIdentity(config({ numberOfChannels: 1 }))).not.toEqual(base);
});

test("AAC and MP3 frame durations follow their codec frame sizes", () => {
	expect(frameDuration(config({ codec: "mp4a.40.2", sampleRate: 48000 }))).toBeCloseTo(21.333, 3);
	expect(frameDuration(config({ codec: "mp4a.40.5", sampleRate: 48000 }))).toBeCloseTo(42.667, 3);
	expect(frameDuration(config({ codec: "mp3", sampleRate: 48000 }))).toBe(Time.Milli(24));
	expect(frameDuration(config({ codec: "mp3", sampleRate: 24000 }))).toBe(Time.Milli(24));
});

test("Opus and unknown codecs have no constant frame duration", () => {
	// Opus states its duration per packet, in the TOC byte.
	expect(frameDuration(config())).toBeUndefined();
	expect(frameDuration(config({ codec: "flac" }))).toBeUndefined();
});

test("an implicit CMAF duration falls through to the Opus TOC", () => {
	// TOC 0x78: config 15 (hybrid fullband, 20 ms), one frame.
	const frame = {
		timestamp: Time.Micro(0),
		keyframe: true,
		payload: new Uint8Array([0x78, 0]),
		duration: Time.Micro(0),
	};
	expect(packetDuration("opus", frame)).toBe(Time.Milli(20));
	expect(packetDuration("mp4a.40.2", frame)).toBeUndefined();
});
