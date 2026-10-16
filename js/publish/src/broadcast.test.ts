import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path, Track } from "@moq/net";
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

test("sibling broadcasts on one page advertise the same capture clock", async () => {
	const origin = new Origin.Producer();
	const camera = new Broadcast({ enabled: true, origin, name: Path.from("participant/camera.hang") });
	const screen = new Broadcast({ enabled: true, origin, name: Path.from("participant/screen.hang") });
	try {
		camera.video("video").config.set(videoConfig);
		screen.video("video").config.set(videoConfig);
		await settle();
		const cameraClock = (await readCatalog(camera))?.clock;
		const screenClock = (await readCatalog(screen))?.clock;
		if (!cameraClock || !screenClock) throw new Error("siblings must advertise their page clock");
		expect(cameraClock).toEqual(screenClock);
		const captured = 1_000_000;
		expect(Catalog.wallClockTime(cameraClock, captured, 1_000_000)).toEqual(
			Catalog.wallClockTime(screenClock, captured, 1_000_000),
		);
	} finally {
		camera.close();
		screen.close();
		origin.close();
	}
});

test("folds video and audio renditions into the catalog by full track name", async () => {
	const rotation = new Signal(90);
	const broadcast = new Broadcast({
		enabled: true,
		display: { width: 1920, height: 1080 },
		flip: true,
		rotation,
	});

	broadcast.video("video/hd").config.set(videoConfig);
	broadcast.audio("audio/data").config.set(audioConfig);
	await settle();

	const catalog = await readCatalog(broadcast);
	expect(catalog?.video?.renditions["video/hd"]?.codec).toBe("avc1.640028");
	// The catalog display is the size to render at, so a quarter turn swaps the captured size.
	expect(Number(catalog?.video?.display?.width)).toBe(1080);
	expect(Number(catalog?.video?.display?.height)).toBe(1920);
	expect(catalog?.video?.flip).toBe(true);
	expect(catalog?.video?.rotation).toBe(90);
	expect(catalog?.audio?.renditions["audio/data"]?.codec).toBe("opus");

	rotation.set(270);
	await settle();
	const turned = await readCatalog(broadcast);
	expect(turned?.video?.rotation).toBe(270);
	expect(Number(turned?.video?.display?.width)).toBe(1080);
	expect(Number(turned?.video?.display?.height)).toBe(1920);

	rotation.set(0);
	await settle();
	const upright = await readCatalog(broadcast);
	expect(upright?.video?.rotation).toBeUndefined();
	expect(Number(upright?.video?.display?.width)).toBe(1920);
	expect(Number(upright?.video?.display?.height)).toBe(1080);

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

// A replacement encoder gets a fresh track name while its catalog ID and broadcast stay stable.
test("replacing a closed rendition track keeps the broadcast and publishes a fresh track name", async () => {
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
	again.config.set(videoConfig);
	await settle();
	const next = broadcast.net.peek();
	expect(next).toBeDefined();
	expect(next).toBe(net);
	if (!next) throw new Error("expected the network producer");

	// The replacement starts its sequences over under the new track name.
	const config = (await readCatalog(broadcast))?.video?.renditions.video;
	if (!config?.track) throw new Error("expected replacement track in catalog");
	expect(config.track).not.toBe("video");
	const second = next.track(config.track).subscribe();
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
	const maxAge = new Signal<Milli | undefined>(Milli(1000));
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
	maxAge.set(Milli(2000));
	await settle();
	const after = broadcast.net.peek();
	expect(after).toBeDefined();
	expect(after).not.toBe(before);
	expect((await after?.track("video").info())?.maxAge).toBe(Milli(2000));

	broadcast.close();
});

test("keeps the current catalog snapshot for a reconnecting viewer", async () => {
	const clock = new Manual();
	const restore = clock.install();

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
		await clock.advanceAsync(Nano.fromMilli(Milli(60_000)));
		const second = net.track(Broadcast.CATALOG_TRACK).subscribe();
		expect((await new Json.Snapshot.Consumer<Catalog.Root>({ track: second }).latest())?.value.video).toBeDefined();
		second.close();

		broadcast.close();
	} finally {
		restore();
	}
});

test("registering a sibling after an encoder failure keeps the network broadcast", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("failure.hang") });
	const video = broadcast.video("video");
	video.config.set(videoConfig);
	await settle();
	const net = broadcast.net.peek();
	if (!net) throw new Error("expected network broadcast");
	const subscriber = net.track("video").subscribe();
	try {
		await settle();
		const track = video.track.peek();
		if (!track) throw new Error("expected demanded video track");
		track.close(new Error("encoder failed"));
		await settle();
		broadcast.audio("audio/new").config.set(audioConfig);
		await settle();
		expect(broadcast.net.peek()).toBe(net);
	} finally {
		subscriber.close();
		broadcast.close();
	}
});

// A name always means the same content, so a served rendition whose decoder identity changes moves to
// a fresh track name under the same catalog ID and epoch, ending the old track on its old identity.
// Fields a decoder needn't reinitialize for update the served track in place.
test("a served decoder identity change moves the rendition to a fresh track name", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("identity.hang") });
	const video = broadcast.video("video");
	const sized = {
		...videoConfig,
		codedWidth: Catalog.u53(1280),
		codedHeight: Catalog.u53(720),
		bitrate: Catalog.u53(1_000_000),
		framerate: 30,
	};
	video.config.set(sized);
	await settle();

	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const epoch = net.route?.epoch;
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({
		track: net.track(Broadcast.CATALOG_TRACK).subscribe(),
	});
	const first = net.track("video").subscribe();
	try {
		expect((await catalog.latest())?.value.video?.renditions.video?.track).toBeUndefined();
		await settle();
		const track = video.track.peek();
		if (!track) throw new Error("expected the demanded track");
		track.appendGroup().close();

		// Bitrate, frame rate, and coded size move either way on the served track.
		for (const update of [
			{ bitrate: Catalog.u53(250_000), framerate: 15 },
			{ codedWidth: Catalog.u53(1920), codedHeight: Catalog.u53(1080), bitrate: Catalog.u53(4_000_000) },
			{ codedWidth: Catalog.u53(640), codedHeight: Catalog.u53(360) },
		]) {
			video.config.set({ ...sized, ...update });
			await settle();
			const config = (await catalog.latest())?.value.video?.renditions.video;
			expect(config).toMatchObject(update);
			expect(config?.track).toBeUndefined();
			expect(video.track.peek()).toBe(track);
		}

		// A codec change is a new identity.
		video.config.set({ ...sized, codec: "vp8" });
		await settle();
		const config = (await catalog.latest())?.value.video?.renditions.video;
		expect(config?.track).toBe("video/1");
		expect(config?.codec).toBe("vp8");
		expect(track.closed.peek()).toBeNull();
		expect(video.track.peek()).toBeUndefined();
		expect(broadcast.net.peek()).toBe(net);
		expect(net.route?.epoch).toBe(epoch);

		const second = net.track("video/1").subscribe();
		await settle();
		const replacement = video.track.peek();
		expect(replacement?.name).toBe("video/1");
		expect(replacement?.appendGroup().sequence).toBe(0);

		// So is a display aspect change, which moves it again rather than redefining the second name.
		video.config.set({
			...sized,
			codec: "vp8",
			displayAspectWidth: Catalog.u53(4),
			displayAspectHeight: Catalog.u53(3),
		});
		await settle();
		expect((await catalog.latest())?.value.video?.renditions.video?.track).toBe("video/2");
		expect(replacement?.closed.peek()).toBeNull();
		second.close();
	} finally {
		first.close();
		broadcast.close();
	}
});

// The counter skips every name this broadcast already uses, including an application's own sections.
test("a minted track name skips names already in the catalog", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("collide.hang") });
	broadcast.audio("video/1").config.set(audioConfig);
	broadcast.catalog.mutate((catalog) => {
		catalog.json = { tracks: { meta: { mode: "stream", track: "video/2" } } };
	});
	const video = broadcast.video("video");
	video.config.set(videoConfig);
	await settle();

	video.config.set({ ...videoConfig, codec: "vp8" });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBe("video/3");

	broadcast.close();
});

// A removed rendition keeps its network track, so its name stays taken even if it never had a config.
test("a minted track name skips a removed rendition's track", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("removed.hang") });
	const removed = broadcast.video("video/1");
	await settle();
	removed.close();
	const video = broadcast.video("video");
	video.config.set(videoConfig);
	await settle();

	video.config.set({ ...videoConfig, codec: "vp8" });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBe("video/2");
	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const subscriber = net.track("video/2").subscribe();
	await settle();
	expect(video.track.peek()?.name).toBe("video/2");

	subscriber.close();
	broadcast.close();
});

// A minted name belongs to its rendition's media, so registering it as an ID fails loud.
test("registering a replacement track's name throws", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("owned.hang") });
	const video = broadcast.video("video");
	video.config.set(videoConfig);
	await settle();
	video.config.set({ ...videoConfig, codec: "vp8" });
	await settle();
	video.config.set({ ...videoConfig, codec: "vp9" });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBe("video/2");

	// Both the current replacement and a retired one stay owned.
	expect(() => broadcast.video("video/2")).toThrow("replacement track");
	expect(() => broadcast.video("video/1")).toThrow("replacement track");
	// A sibling still registers and serves.
	broadcast.audio("audio").config.set(audioConfig);
	await settle();
	expect((await readCatalog(broadcast))?.audio?.renditions.audio?.codec).toBe("opus");

	broadcast.close();
});

// A replacement minted before its rendition ever had a config is never served, but still owns its name.
test("a replacement track name is never reused, even one never served", async () => {
	const broadcast = new Broadcast({ enabled: true, origin: new Origin.Producer(), name: Path.from("unserved.hang") });
	let video = broadcast.video("video");
	await settle();
	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");

	// Fail the rendition on its current track, then register a replacement for it.
	const fail = async (name: string) => {
		const sub = net.track(name).subscribe();
		await settle();
		video.track.peek()?.close(new Error("encoder failed"));
		video.close();
		await settle();
		sub.close();
		video = broadcast.video("video");
		await settle();
	};
	await fail("video");
	await fail("video/1");
	await fail("video/2");
	expect(net.track("video/3").subscribe()).toBeDefined();
	expect(() => broadcast.video("video/1")).toThrow("replacement track");

	video.config.set(videoConfig);
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBe("video/3");

	broadcast.close();
});

test("a rendition config pointing at another track is refused", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("remote.hang") });
	const video = broadcast.video("video");
	video.config.set(videoConfig);
	await settle();

	// A remote reference would send viewers to media this publisher never writes.
	video.config.set({
		...videoConfig,
		codec: "vp8",
		broadcast: "source" as Catalog.RelativeBroadcast,
		track: "video",
	});
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.codec).toBe(videoConfig.codec);

	video.config.set({ ...videoConfig, codec: "vp8" });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.codec).toBe("vp8");

	broadcast.close();
});

// The served identity is a snapshot, so a producer mutating a container it shares can't rewrite it.
test("mutating a shared container still takes a fresh track name", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("shared.hang") });
	const video = broadcast.video("video");
	const container = { kind: "cmaf" as const, init: "AAAA" };
	video.config.set({ ...videoConfig, container });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBeUndefined();

	container.init = "BBBB";
	video.config.set({ ...videoConfig, container, bitrate: Catalog.u53(1_000_000) });
	await settle();
	expect((await readCatalog(broadcast))?.video?.renditions.video?.track).toBe("video/1");

	broadcast.close();
});

test("a name returning with a different identity takes a fresh track name", async () => {
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("returning.hang") });
	let audio = broadcast.audio("audio");
	audio.config.set(audioConfig);
	await settle();
	expect((await readCatalog(broadcast))?.audio?.renditions.audio?.track).toBeUndefined();

	// The same identity returns to its track, even at a lower bitrate.
	audio.close();
	await settle();
	audio = broadcast.audio("audio");
	audio.config.set({ ...audioConfig, bitrate: Catalog.u53(32_000) });
	await settle();
	expect((await readCatalog(broadcast))?.audio?.renditions.audio?.track).toBeUndefined();

	audio.close();
	await settle();
	audio = broadcast.audio("audio");
	audio.config.set({ ...audioConfig, numberOfChannels: Catalog.u53(1) });
	await settle();
	expect((await readCatalog(broadcast))?.audio?.renditions.audio?.track).toBe("audio/1");

	broadcast.close();
});

// Nobody has seen an unserved catalog, so its renditions settle without minting names.
test("identity changes before the first announce keep the catalog ID as the track name", async () => {
	const announce = new Signal(false);
	const broadcast = new Broadcast({ origin: new Origin.Producer(), name: Path.from("unserved.hang"), announce });
	const video = broadcast.video("video");
	video.config.set({ ...videoConfig, bitrate: Catalog.u53(1_000_000) });
	await settle();
	video.config.set({ ...videoConfig, codec: "vp8", bitrate: Catalog.u53(500_000) });
	await settle();

	announce.set(true);
	await settle();
	const net = broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({
		track: net.track(Broadcast.CATALOG_TRACK).subscribe(),
	});
	const config = (await catalog.latest())?.value.video?.renditions.video;
	expect(config?.codec).toBe("vp8");
	expect(config?.bitrate).toBe(Catalog.u53(500_000));
	expect(config?.track).toBeUndefined();

	// The first snapshot fixed the name's identity.
	video.config.set(videoConfig);
	await settle();
	expect((await catalog.latest())?.value.video?.renditions.video?.track).toBe("video/1");

	broadcast.close();
});
