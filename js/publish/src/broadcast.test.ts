import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path, Time, Track } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
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
	const catalog = (await new Json.Snapshot.Consumer<Catalog.Root>({ track: track.subscribe() }).latest())?.value;
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

test("subscriber demand hands the producer to the rendition and clears it when the track closes", async () => {
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name: Path.from("test.hang") });
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	const rendition = broadcast.video("video");
	await settle();
	const subscriber = net.track("video").subscribe();
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

test("serves the catalog through a shared static track", async () => {
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name: Path.from("test.hang") });
	broadcast.video("video").config.set(videoConfig);
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	const subscriber = net.track(Broadcast.CATALOG_TRACK).subscribe();
	const catalog = (await new Json.Snapshot.Consumer<Catalog.Root>({ track: subscriber }).latest())?.value;
	expect(catalog?.video?.renditions.video?.codec).toBe("avc1.640028");

	// Dropping the subscriber closes the served track; the broadcast keeps running for the next viewer.
	subscriber.close();
	await settle();
	expect(broadcast.net.peek()).toBe(net);

	broadcast.close();
});

// Regression: the catalog was served from the moment the origin existed, so the snapshots published
// while renditions resolved stayed on the track and a subscriber could start from a partial one.
test("serves no catalog until announced, so the first snapshot is the one at announce time", async () => {
	const announce = new Signal(false);
	const broadcast = new Broadcast({
		enabled: true,
		origin: new Origin.Producer(),
		name: Path.from("test.hang"),
		announce,
	});
	await settle();

	// Renditions resolve in separate ticks while unannounced.
	broadcast.video("video").config.set(videoConfig);
	await settle();
	broadcast.audio("audio").config.set(audioConfig);
	await settle();

	announce.set(true);
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	const group = await net.track(Broadcast.CATALOG_TRACK).subscribe().ordered().nextGroup();
	expect(group?.sequence).toBe(0);
	const catalog = (await group?.readJson()) as Catalog.Root;
	expect(Object.keys(catalog.video?.renditions ?? {})).toEqual(["video"]);
	expect(Object.keys(catalog.audio?.renditions ?? {})).toEqual(["audio"]);

	broadcast.close();
});

// Regression: a re-announce re-created the catalog track, restarting its group sequence under the
// same epoch, so a resumed viewer mistook the new catalog for the old one.
test("a re-announce continues the catalog's group sequence", async () => {
	const announce = new Signal(true);
	const broadcast = new Broadcast({
		enabled: true,
		origin: new Origin.Producer(),
		name: Path.from("test.hang"),
		announce,
	});
	broadcast.video("video").config.set(videoConfig);
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const first = net.track(Broadcast.CATALOG_TRACK).subscribe().ordered();
	expect((await first.nextGroup())?.sequence).toBe(0);

	announce.set(false);
	await settle();
	broadcast.audio("audio").config.set(audioConfig);
	await settle();
	announce.set(true);
	await settle();

	// The same track keeps serving, and its newest group is past the first.
	expect(broadcast.net.peek()).toBe(net);
	const group = await first.nextGroup();
	expect(group?.sequence).toBeGreaterThan(0);
	const catalog = (await group?.readJson()) as Catalog.Root;
	expect(Object.keys(catalog.audio?.renditions ?? {})).toEqual(["audio"]);

	first.close();
	broadcast.close();
});

// Regression: any rendition change re-created every rendition's track, so the next append restarted
// a live track's group sequence.
test("a rendition keeps its track across rendition changes", async () => {
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name: Path.from("test.hang") });
	const video = broadcast.video("video");
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const subscriber = net.track("video").subscribe();
	await settle();
	const track = video.track.peek();
	if (!track) throw new Error("expected the demanded track");
	track.appendGroup().close();

	// Registering another rendition leaves this one's track alone.
	const audio = broadcast.audio("audio");
	await settle();
	expect(video.track.peek()).toBe(track);

	// So does closing a rendition and registering its name again.
	video.close();
	await settle();
	const again = broadcast.video("video");
	await settle();
	expect(again.track.peek()).toBe(track);
	expect(track.appendGroup().sequence).toBe(1);

	// The name keeps its kind.
	again.close();
	expect(() => broadcast.audio("video")).toThrow("registered as video");

	audio.close();
	subscriber.close();
	broadcast.close();
});

test("a rendition name may change kind on a new network broadcast", async () => {
	const name = new Signal(Path.from("one.hang"));
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name });
	const video = broadcast.video("x");
	await settle();
	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const subscriber = net.track("x").subscribe();
	await settle();
	video.close();
	expect(() => broadcast.audio("x")).toThrow("registered as video");

	// A new network broadcast holds no track for the name, so any kind may take it.
	name.set(Path.from("two.hang"));
	await settle();
	expect(() => broadcast.audio("x")).not.toThrow();

	subscriber.close();
	broadcast.close();
});

// A closed track can't be continued, and a replacement under its name would restart its sequences,
// so the replacement goes out on a new network broadcast.
test("replacing a closed rendition track publishes a new network broadcast", async () => {
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name: Path.from("test.hang") });
	const video = broadcast.video("video");
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const first = net.track("video").subscribe();
	await settle();
	const track = video.track.peek();
	if (!track) throw new Error("expected the demanded track");

	// The encoder wrote, then failed.
	track.appendGroup().close();
	track.close(new Error("encoder failed"));
	video.close();
	await settle();

	const again = broadcast.video("video");
	await settle();
	const next = broadcast.net.peek();
	expect(next).toBeDefined();
	expect(next).not.toBe(net);
	if (!next) throw new Error("expected a new network producer");

	// The replacement starts the name over on the new broadcast.
	const second = next.track("video").subscribe();
	await settle();
	const replacement = again.track.peek();
	expect(replacement).toBeDefined();
	expect(replacement).not.toBe(track);
	expect(replacement?.appendGroup().sequence).toBe(0);

	first.close();
	second.close();
	broadcast.close();
});

test("changing maxAge publishes a new network broadcast", async () => {
	const maxAge = new Signal<Time.Milli | undefined>(Time.Milli(1000));
	const broadcast = new Broadcast({
		enabled: true,
		origin: new Origin.Producer(),
		name: Path.from("test.hang"),
		maxAge,
	});
	broadcast.video("video");
	await settle();
	const before = broadcast.net.peek();

	// A track's retention is fixed at creation, so the tracks move to a fresh broadcast.
	maxAge.set(Time.Milli(2000));
	await settle();
	const after = broadcast.net.peek();
	expect(after).toBeDefined();
	expect(after).not.toBe(before);
	expect((await after?.track("video").info())?.maxAge).toBe(Time.Milli(2000));

	broadcast.close();
});

test("keeps the current catalog snapshot for a reconnecting viewer", async () => {
	const real = performance.now.bind(performance);
	let now = real();
	performance.now = () => now;

	try {
		const broadcast = new Broadcast({
			enabled: true,
			origin: new Origin.Producer(),
			name: Path.from("test.hang"),
		});
		broadcast.video("video").config.set(videoConfig);
		await settle();

		const net = broadcast.net.peek();
		if (!net) throw new Error("expected a network producer once connected");

		const first = net.track(Broadcast.CATALOG_TRACK).subscribe();
		expect((await new Json.Snapshot.Consumer<Catalog.Root>({ track: first }).latest())?.value.video).toBeDefined();
		first.close();

		// A reconnect past the idle cache window must still receive the live track's newest
		// snapshot instead of waiting forever for an edit that may never come.
		now += 60_000;
		const second = net.track(Broadcast.CATALOG_TRACK).subscribe();
		expect((await new Json.Snapshot.Consumer<Catalog.Root>({ track: second }).latest())?.value.video).toBeDefined();
		second.close();

		broadcast.close();
	} finally {
		performance.now = real;
	}
});
