import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Effect, Signal } from "@moq/signals";
import { playbackIdentity } from "./config";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function config(fields: Record<string, unknown> = {}): Catalog.VideoConfig {
	return Catalog.VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
		description: "0142002a",
		optimizeForLatency: true,
		...fields,
	});
}

test("metadata changes do not change the playback identity", async () => {
	const rendition = new Signal<Catalog.VideoConfig>(config());
	const root = new Effect();
	const identity = root.computed((effect) => playbackIdentity(effect.get(rendition)));
	let runs = 0;

	root.run((effect) => {
		effect.get(identity);
		runs++;
	});
	await flush();
	expect(runs).toBe(1);

	rendition.set(
		config({
			bitrate: 2_000_000,
			stalled: true,
			codedWidth: 1280,
			codedHeight: 720,
			framerate: 30,
			jitter: 33,
		}),
	);
	await flush();
	expect(runs).toBe(1);

	rendition.set(config({ codec: "avc1.640028" }));
	await flush();
	expect(runs).toBe(2);

	root.close();
});

test("routing and decoder inputs change the playback identity", () => {
	const base = playbackIdentity(config());

	expect(playbackIdentity(config({ broadcast: "../source" }))).not.toEqual(base);
	expect(playbackIdentity(config({ description: "0164001f" }))).not.toEqual(base);
	expect(playbackIdentity(config({ container: { kind: "loc" } }))).not.toEqual(base);
	expect(playbackIdentity(config({ displayAspectWidth: 16, displayAspectHeight: 9 }))).not.toEqual(base);
	expect(playbackIdentity(config({ optimizeForLatency: false }))).not.toEqual(base);
});
