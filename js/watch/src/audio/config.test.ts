import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { playbackIdentity, playbackJitter } from "./config";

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

test("metadata changes do not change the playback identity", async () => {
	const rendition = new Signal<Catalog.AudioConfig>(config());
	const root = new Effect();
	const identity = root.computed((effect) => playbackIdentity(effect.get(rendition)));
	let worklets = 0;

	root.run((effect) => {
		effect.get(identity);
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

test("an advertised jitter of zero falls back to the codec frame duration", () => {
	// 48kHz stereo Opus: 20ms frames plus the 128 sample worklet quantum (3ms).
	const floor = playbackJitter(config());
	expect(floor).toBe(Time.Milli(23));
	expect(playbackJitter(config({ jitter: 0 }))).toBe(floor);
	expect(playbackJitter(config({ jitter: 60 }))).toBe(Time.Milli(63));
});

test("an unknown codec without advertised jitter only reserves the worklet quantum", () => {
	expect(playbackJitter(config({ codec: "flac", jitter: 0 }))).toBe(Time.Milli(3));
});
