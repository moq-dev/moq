import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { Broadcast } from "./broadcast.ts";

// Effects and signal writes coalesce onto microtasks, so a chain of registration -> config -> catalog
// needs a few flushes to settle.
const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

// Read the current catalog by seeding a fresh subscriber (CatalogProducer seeds each one).
async function readCatalog(broadcast: Broadcast): Promise<Catalog.Root | undefined> {
	const effect = new Effect();
	const track = new Track.Producer("catalog.json");
	broadcast.catalog.serve(track, effect);
	const catalog = await new Json.Snapshot.Consumer<Catalog.Root>(track.subscribe()).next();
	effect.close();
	return catalog;
}

const videoConfig: Catalog.VideoConfig = { codec: "avc1.640028", container: { kind: "legacy" } };
const audioConfig: Catalog.AudioConfig = {
	codec: "opus",
	sampleRate: Catalog.u53(48000),
	numberOfChannels: Catalog.u53(2),
	container: { kind: "legacy" },
};

test("folds video and audio renditions into the catalog by full track name", async () => {
	const broadcast = new Broadcast({ enabled: true, display: { width: 1920, height: 1080 }, flip: true });

	broadcast.video("video/hd").config.set(videoConfig);
	broadcast.audio("audio/data").config.set(audioConfig);
	await settle();

	const catalog = await readCatalog(broadcast);
	expect(catalog?.video?.renditions["video/hd"]?.codec).toBe("avc1.640028");
	expect(Number(catalog?.video?.display?.width)).toBe(1920);
	expect(Number(catalog?.video?.display?.height)).toBe(1080);
	expect(catalog?.video?.flip).toBe(true);
	expect(catalog?.audio?.renditions["audio/data"]?.codec).toBe("opus");

	broadcast.close();
});

test("a rendition with an undefined config is omitted from the catalog", async () => {
	const broadcast = new Broadcast({ enabled: true });

	const hd = broadcast.video("video/hd");
	const sd = broadcast.video("video/sd");
	hd.config.set(videoConfig);
	sd.config.set(videoConfig);
	await settle();

	let catalog = await readCatalog(broadcast);
	expect(Object.keys(catalog?.video?.renditions ?? {})).toEqual(["video/hd", "video/sd"]);

	// Clearing one config drops just that entry.
	sd.config.set(undefined);
	await settle();
	catalog = await readCatalog(broadcast);
	expect(Object.keys(catalog?.video?.renditions ?? {})).toEqual(["video/hd"]);

	// Clearing the last leaves no defined configs, so the whole section is deleted.
	hd.config.set(undefined);
	await settle();
	catalog = await readCatalog(broadcast);
	expect(catalog?.video).toBeUndefined();

	broadcast.close();
});

test("a duplicate track name throws across both kinds", () => {
	const broadcast = new Broadcast({ enabled: true });

	broadcast.video("video/hd");
	expect(() => broadcast.video("video/hd")).toThrow();
	// The single registry enforces uniqueness across video and audio.
	expect(() => broadcast.audio("video/hd")).toThrow();

	broadcast.close();
});

test("rendition.close() unregisters the name and drops it from the catalog", async () => {
	const broadcast = new Broadcast({ enabled: true });

	const hd = broadcast.video("video/hd");
	hd.config.set(videoConfig);
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions["video/hd"]).toBeDefined();

	hd.close();
	await settle();
	expect((await readCatalog(broadcast))?.video).toBeUndefined();

	// The name is free to register again.
	expect(() => broadcast.video("video/hd")).not.toThrow();

	broadcast.close();
});
