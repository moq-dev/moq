import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { TrackProducer } from "@moq/net";
import { Effect } from "@moq/signals";
import { CatalogProducer } from "./catalog.ts";

const videoConfig: Catalog.VideoConfig = {
	codec: "vp8",
	container: { kind: "legacy" },
	codedWidth: Catalog.u53(1280),
	codedHeight: Catalog.u53(720),
};

const audioConfig: Catalog.AudioConfig = {
	codec: "opus",
	container: { kind: "legacy" },
	sampleRate: Catalog.u53(48_000),
	numberOfChannels: Catalog.u53(2),
};

async function settled<T>(promise: Promise<T>): Promise<"pending" | T> {
	return await Promise.race([promise, new Promise<"pending">((resolve) => setTimeout(() => resolve("pending"), 0))]);
}

test("catalog producer seeds subscribers and fans out edits", async () => {
	const catalog = new CatalogProducer();

	// Edit before anyone subscribes: the value is retained, not lost.
	catalog.mutate((c) => {
		c.video = { renditions: {} };
	});

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track.subscribe());

	// A new subscriber is seeded with the current catalog.
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	// An extension owner adds its own section; the subscriber sees the update, video untouched.
	catalog.mutate((c) => {
		c.scte35 = { splices: [] };
	});
	const update = await consumer.next();
	expect(update?.video).toEqual({ renditions: {} });
	expect(update?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("catalog reservation withholds the first snapshot", async () => {
	const catalog = new CatalogProducer();
	const reserved = catalog.reserve();

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track.subscribe());
	const next = consumer.next();

	catalog.mutate((c) => {
		c.video = { renditions: { "video/hd": videoConfig } };
	});
	expect(await settled(next)).toBe("pending");

	reserved.close();
	expect((await next)?.video).toEqual({ renditions: { "video/hd": videoConfig } });

	effect.close();
});

test("reserved renditions publish one complete first snapshot", async () => {
	const catalog = new CatalogProducer();
	const reserved = catalog.reserve();
	const video = reserved.video("video/hd");
	const audio = reserved.audio("audio/data");
	reserved.close();

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track.subscribe());
	const next = consumer.next();

	video.set(videoConfig);
	expect(await settled(next)).toBe("pending");

	audio.set(audioConfig);
	const first = await next;
	expect(first?.video).toEqual({ renditions: { "video/hd": videoConfig } });
	expect(first?.audio).toEqual({ renditions: { "audio/data": audioConfig } });

	video.update((config) => {
		config.bitrate = Catalog.u53(1_000_000);
	});
	expect((await consumer.next())?.video?.renditions["video/hd"]?.bitrate).toBe(Catalog.u53(1_000_000));

	audio.close();
	expect((await consumer.next())?.audio).toBeUndefined();

	video.close();
	effect.close();
});

test("untouched reservations do not publish an empty catalog", async () => {
	const catalog = new CatalogProducer();
	const reserved = catalog.reserve();

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track.subscribe());
	const next = consumer.next();

	reserved.close();
	expect(await settled(next)).toBe("pending");

	catalog.mutate((c) => {
		c.scte35 = { splices: [] };
	});
	expect(await next).toEqual({ scte35: { splices: [] } });

	effect.close();
});

test("removing the last rendition keeps a section with display metadata", async () => {
	const catalog = new CatalogProducer();
	const reserved = catalog.reserve();
	const video = reserved.video("video/hd");
	reserved.close();

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const consumer = new Json.Consumer<Catalog.Root>(track.subscribe());
	const first = consumer.next();

	video.set(videoConfig);
	await first;

	// Display metadata lives outside the rendition guard, so removing the last rendition must keep it.
	catalog.mutate((c) => {
		const section = c.video as {
			renditions: Record<string, Catalog.VideoConfig>;
			display: { width: number; height: number };
		};
		section.display = { width: 1920, height: 1080 };
	});
	await consumer.next();

	video.close();
	expect(await consumer.next()).toEqual({ video: { renditions: {}, display: { width: 1920, height: 1080 } } });

	effect.close();
});

test("catalog producer publishes every update as a snapshot group", async () => {
	const catalog = new CatalogProducer();
	catalog.mutate((c) => {
		c.video = { renditions: {} };
	});

	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const subscriber = track.subscribe();

	const first = await subscriber.nextGroup();
	expect(first?.sequence).toBe(0);
	expect(await first?.readJson()).toEqual({ video: { renditions: {} } });
	expect(first?.done).toBe(true);

	catalog.mutate((c) => {
		c.scte35 = { splices: [] };
	});

	const second = await subscriber.nextGroup();
	expect(second?.sequence).toBe(1);
	expect(await second?.readJson()).toEqual({ video: { renditions: {} }, scte35: { splices: [] } });
	expect(second?.done).toBe(true);

	effect.close();
});

test("a reconnecting subscriber is seeded with the full current catalog", async () => {
	const catalog = new CatalogProducer();
	catalog.mutate((c) => {
		c.video = { renditions: {} };
		c.scte35 = { splices: [] };
	});

	// The first subscription drains and ends...
	const first = new Effect();
	catalog.serve(new TrackProducer("catalog.json"), first);
	first.close();

	// ...and a fresh subscription still gets the current catalog, not nothing.
	const effect = new Effect();
	const track = new TrackProducer("catalog.json");
	catalog.serve(track, effect);
	const seeded = await new Json.Consumer<Catalog.Root>(track.subscribe()).next();
	expect(seeded?.video).toEqual({ renditions: {} });
	expect(seeded?.scte35).toEqual({ splices: [] });

	effect.close();
});
