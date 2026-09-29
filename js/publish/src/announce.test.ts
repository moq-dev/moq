import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Origin, Path } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Announce from "./announce";
import { Broadcast } from "./broadcast";
import type { AnnounceMode } from "./element";

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));
async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

const videoConfig: Catalog.VideoConfig = { codec: "avc1.640028", container: { kind: "legacy" } };
const audioConfig: Catalog.AudioConfig = {
	codec: "opus",
	sampleRate: Catalog.u53(48000),
	numberOfChannels: Catalog.u53(2),
	container: { kind: "legacy" },
};

// `<moq-publish>`'s wiring, with each encoder reduced to its rendition and `settled` flag.
function setup() {
	const effect = new Effect();
	const announcing = new Signal(false);
	const broadcast = new Broadcast({
		enabled: true,
		origin: new Origin.Producer(),
		name: Path.from("test.hang"),
		announce: announcing,
	});
	const track = () => ({
		enabled: new Signal(true),
		source: new Signal<object | undefined>(undefined),
		settled: new Signal(false),
	});
	const video = { ...track(), rendition: broadcast.video("video") };
	const audio = { ...track(), rendition: broadcast.audio("audio") };

	Announce.run(effect, announcing, {
		mode: new Signal<AnnounceMode>("source"),
		camera: new Signal(true),
		video,
		audio,
	});

	return {
		announcing,
		broadcast,
		video,
		audio,
		[Symbol.dispose]() {
			effect.close();
			broadcast.close();
		},
	};
}

// Regression: the connection comes up before getUserMedia resolves, and each encoder resolves its
// config in its own tick. Announcing once the sources arrived let a subscriber lock onto a catalog
// missing a rendition, which a one-shot consumer (export, HLS, a recording) can't recover from.
test("a subscriber arriving at announce time sees every rendition in its first catalog", async () => {
	using env = setup();
	await settle();
	expect(env.announcing.peek()).toBe(false);

	// getUserMedia resolves: both tracks are captured, but neither config has resolved yet.
	env.video.source.set({});
	env.audio.source.set({});
	await settle();
	expect(env.announcing.peek()).toBe(false);

	env.video.rendition.config.set(videoConfig);
	env.video.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(false);

	env.audio.rendition.config.set(audioConfig);
	env.audio.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);

	const net = env.broadcast.net.peek();
	if (!net) throw new Error("expected a network producer once connected");
	const group = await net.track(Broadcast.CATALOG_TRACK).subscribe().ordered().nextGroup();
	expect(group?.sequence).toBe(0);
	const catalog = (await group?.readJson()) as Catalog.Root;
	expect(Object.keys(catalog.video?.renditions ?? {})).toEqual(["video"]);
	expect(Object.keys(catalog.audio?.renditions ?? {})).toEqual(["audio"]);

	// Once announced, a later drop (a device swap) edits the catalog instead of unannouncing.
	env.video.rendition.config.set(undefined);
	env.video.settled.set(false);
	await settle();
	expect(env.announcing.peek()).toBe(true);
});

test("a track that can't resolve doesn't hold the rest back", async () => {
	using env = setup();
	env.video.source.set({});
	env.audio.source.set({});
	env.video.rendition.config.set(videoConfig);
	env.video.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(false);

	// e.g. the microphone waits for the page's first gesture, or its encoder failed.
	env.audio.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);
});

test("a disabled track isn't waited on", async () => {
	using env = setup();
	env.audio.enabled.set(false);
	env.video.source.set({});
	await settle();
	expect(env.announcing.peek()).toBe(false);

	env.video.rendition.config.set(videoConfig);
	env.video.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);
});
