import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Effect, Signal } from "@moq/signals";
import { decoderConfig, decoderDescription, frameDuration, packetDuration, playbackIdentity } from "./config";

const flush = () => yieldNow();

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
	for (const codec of ["mp4a.40.2", "mp4a.40.02", "mp4a.67"]) {
		expect(frameDuration(config({ codec, sampleRate: 48000 }))?.asMillis()).toBeCloseTo(21.333, 3);
	}
	for (const codec of ["mp4a.40.5", "mp4a.40.05", "mp4a.40.29"]) {
		expect(frameDuration(config({ codec, sampleRate: 48000 }))?.asMillis()).toBeCloseTo(42.667, 3);
	}
	expect(frameDuration(config({ codec: "mp3", sampleRate: 48000 }))).toEqual(Duration.millis(24));
	expect(frameDuration(config({ codec: "mp3", sampleRate: 24000 }))).toEqual(Duration.millis(24));
});

test("an AAC frame with an AudioSpecificConfig runs 1024 samples at its core's rate", () => {
	// A 48 kHz core with SBR at 96 kHz, SBR at the core's own 48 kHz, and plain AAC-LC.
	for (const [description, sampleRate] of [
		["119056e580", 96000],
		["119056e598", 48000],
		["1190", 48000],
	] as const) {
		const aac = config({ codec: "mp4a.40.2", sampleRate, description });
		expect(frameDuration(aac)?.asMillis()).toBeCloseTo(21.333, 3);
	}
});

test("Opus and unknown codecs have no constant frame duration", () => {
	// Opus states its duration per packet, in the TOC byte.
	expect(frameDuration(config())).toBeUndefined();
	expect(frameDuration(config({ codec: "flac" }))).toBeUndefined();
	// An MP4 audio string the AAC registration does not name: MP3-in-MP4 is not 1024 samples.
	expect(frameDuration(config({ codec: "mp4a.40.34" }))).toBeUndefined();
});

test("an implicit CMAF duration falls through to the Opus TOC", () => {
	// TOC 0x78: config 15 (hybrid fullband, 20 ms), one frame.
	const frame = {
		timestamp: Duration.micros(0),
		keyframe: true,
		payload: new Uint8Array([0x78, 0]),
		duration: Duration.micros(0),
	};
	expect(packetDuration("opus", frame)).toEqual(Duration.millis(20));
	expect(packetDuration("mp4a.40.2", frame)).toBeUndefined();
});

test("an Opus description reaches WebCodecs with its mapping and no pre-skip", () => {
	// OpusHead, version 1, 4 channels, pre-skip 312, 48 kHz, gain 0, family 2 with 4 streams.
	const head = new Uint8Array([
		...new TextEncoder().encode("OpusHead"),
		1,
		4,
		0x38,
		0x01,
		0x80,
		0xbb,
		0,
		0,
		0,
		0,
		2,
		4,
		0,
		0,
		1,
		2,
		3,
	]);
	const description = decoderDescription("opus", head);
	expect(description?.slice(10, 12)).toEqual(new Uint8Array([0, 0]));
	expect([...(description ?? [])].filter((_, index) => index < 10 || index > 11)).toEqual(
		[...head].filter((_, index) => index < 10 || index > 11),
	);
	// The catalog's bytes are left alone.
	expect(head[10]).toBe(0x38);

	// WebCodecs reads only a signed OpusHead, so a dOps stays dropped, as a truncated header fails.
	const dops = new Uint8Array([0, 2, 0x01, 0x38, 0, 0, 0xbb, 0x80, 0, 0, 0]);
	expect(decoderDescription("opus", dops)).toBeUndefined();
	expect(() => decoderDescription("opus", head.slice(0, 12))).toThrow();
	expect(decoderDescription("opus", undefined)).toBeUndefined();
	const asc = new Uint8Array([0x12, 0x10]);
	expect(decoderDescription("mp4a.40.2", asc)).toBe(asc);
});
