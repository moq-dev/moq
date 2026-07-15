import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { type Connection, Path, Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { Broadcast } from "./broadcast.ts";

// The broadcast only opens its network producer once it has a connection, so tests that drive the
// request loop hand it a stub whose publish() is a no-op; the internal producer is exposed via `net`.
const stubConnection = () => ({ publish() {} }) as unknown as Connection.Established;

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

test("serving a subscription hands the producer to the rendition and clears it when the track closes", async () => {
	const broadcast = new Broadcast({ enabled: true, connection: stubConnection(), name: Path.from("test.hang") });
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	const rendition = broadcast.video("video");
	const subscriber = net.subscribe("video");
	await settle();

	// The request loop accepted the subscription and handed the producer to the rendition.
	const track = rendition.track.peek();
	expect(track).toBeDefined();

	// Closing the producer (encoder error / teardown) clears the signal, with no lingering per-subscription
	// effect watching it.
	track?.close();
	await settle();
	expect(rendition.track.peek()).toBeUndefined();

	subscriber.close();
	broadcast.close();
});

test("serves the catalog through the request loop and releases the scope when the subscriber leaves", async () => {
	const broadcast = new Broadcast({ enabled: true, connection: stubConnection(), name: Path.from("test.hang") });
	broadcast.video("video").config.set(videoConfig);
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	// Subscribing to the catalog track drives the per-subscription serving scope.
	const subscriber = net.subscribe(Broadcast.CATALOG_TRACK);
	const catalog = await new Json.Snapshot.Consumer<Catalog.Root>(subscriber).next();
	expect(catalog?.video?.renditions.video?.codec).toBe("avc1.640028");

	// Dropping the subscriber closes the served track; the broadcast keeps running for the next viewer.
	subscriber.close();
	await settle();
	expect(broadcast.net.peek()).toBe(net);

	broadcast.close();
});
