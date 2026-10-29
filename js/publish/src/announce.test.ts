import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Announce from "./announce";
import { Broadcast } from "./broadcast";
import type { AnnounceMode, SourceType } from "./element";

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
function setup(initial: SourceType = "camera") {
	const effect = new Effect();
	const announcing = new Signal(false);
	const connected = new Signal(true);
	const source = new Signal<SourceType | undefined>(initial);
	const mode = new Signal<AnnounceMode | undefined>("source");
	const broadcast = new Broadcast({
		enabled: connected,
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
		mode,
		source: effect.computed((effect) => (effect.get(connected) ? effect.get(source) : undefined)),
		video,
		audio,
	});

	return {
		announcing,
		connected,
		source,
		mode,
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

	// Video alone cannot advertise a partial camera publication.
	env.video.source.set({});
	env.video.rendition.config.set(videoConfig);
	env.video.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(false);

	// getUserMedia resolves for audio, but its config has not resolved yet.
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

	// Capture clears the source while getUserMedia opens the replacement device.
	env.video.source.set(undefined);
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

async function start(env: ReturnType<typeof setup>) {
	env.video.source.set({});
	env.audio.source.set({});
	env.video.rendition.config.set(videoConfig);
	env.audio.rendition.config.set(audioConfig);
	env.video.settled.set(true);
	env.audio.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);
}

test.each(["audio", "video"] as const)("enabling %s keeps the announcement while capture opens", async (kind) => {
	using env = setup();
	await start(env);
	const track = env[kind];
	track.enabled.set(false);
	track.source.set(undefined);
	track.rendition.config.set(undefined);
	track.settled.set(false);
	await settle();
	expect(env.announcing.peek()).toBe(true);

	// Unmute/unhide enables capture before the new getUserMedia promise resolves.
	track.enabled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);
});

test("capture gaps keep the broadcast epoch and same-identity track sequences", async () => {
	using env = setup();
	await start(env);
	const net = env.broadcast.net.peek();
	if (!net) throw new Error("expected a network producer");
	const request = env.broadcast.in.origin.peek()?.follow(env.broadcast.in.name.peek());
	if (!request) throw new Error("expected a follower");
	const epoch = request.active.peek()?.epoch;
	const active: boolean[] = [];
	const stop = request.active.subscribe((value) => active.push(value !== undefined));
	const media = net.track("audio").subscribe();
	const catalogTrack = net.track(Broadcast.CATALOG_TRACK).subscribe();
	const catalog = new Json.Snapshot.Consumer<Catalog.Root>({ track: catalogTrack });
	try {
		await catalog.latest();
		await settle();
		const track = env.audio.rendition.track.peek();
		if (!track) throw new Error("expected the demanded audio track");
		expect(track.appendGroup().sequence).toBe(0);

		// Both devices may be absent during a swap or retry, even though both remain selected.
		env.video.source.set(undefined);
		env.audio.source.set(undefined);
		env.video.rendition.config.set(undefined);
		env.audio.rendition.config.set(undefined);
		env.video.settled.set(false);
		env.audio.settled.set(false);
		await settle();
		expect(env.announcing.peek()).toBe(true);
		expect(request.active.peek()?.epoch).toBe(epoch);

		await start(env);
		expect(env.broadcast.net.peek()).toBe(net);
		expect(env.audio.rendition.track.peek()).toBe(track);
		expect(track.appendGroup().sequence).toBe(1);

		// A different device format uses the broadcast's existing fresh-track mechanism.
		env.audio.rendition.config.set({ ...audioConfig, sampleRate: Catalog.u53(44100) });
		await settle();
		expect((await catalog.latest())?.value.audio?.renditions.audio?.track).toBe("audio/1");
		expect(track.closed.peek()).toBeNull();
		expect(request.active.peek()?.epoch).toBe(epoch);
		expect(active).not.toContain(false);
	} finally {
		stop();
		request.close();
		media.close();
		catalogTrack.close();
	}
});

const boundaries = {
	selection: (env: ReturnType<typeof setup>, on: boolean) => env.source.set(on ? "camera" : undefined),
	connection: (env: ReturnType<typeof setup>, on: boolean) => env.connected.set(on),
};

test.each(Object.keys(boundaries) as (keyof typeof boundaries)[])(
	"clearing the %s resets the first-announce guard",
	async (boundary) => {
		using env = setup();
		await start(env);
		boundaries[boundary](env, false);
		env.video.source.set(undefined);
		env.audio.source.set(undefined);
		env.video.settled.set(false);
		env.audio.settled.set(false);
		await settle();
		expect(env.announcing.peek()).toBe(false);

		boundaries[boundary](env, true);
		await settle();
		expect(env.announcing.peek()).toBe(false);
		env.video.source.set({});
		env.video.settled.set(true);
		await settle();
		expect(env.announcing.peek()).toBe(false);
		env.audio.source.set({});
		env.audio.settled.set(true);
		await settle();
		expect(env.announcing.peek()).toBe(true);
	},
);

test("an invalid mode stops a latched announcement while always needs no selection", async () => {
	using env = setup();
	await start(env);
	env.mode.set(undefined);
	await settle();
	expect(env.announcing.peek()).toBe(false);
	env.source.set(undefined);
	env.mode.set("always");
	await settle();
	expect(env.announcing.peek()).toBe(true);
});

test.each(["screen", "file"] as const)("%s retracts when its captured media ends", async (source) => {
	using env = setup(source);
	await start(env);
	env.video.source.set(undefined);
	env.audio.source.set(undefined);
	env.video.settled.set(false);
	env.audio.settled.set(false);
	await settle();
	expect(env.announcing.peek()).toBe(false);
});

test.each(["screen", "file"] as const)("switching from %s to camera waits for the camera", async (from) => {
	using env = setup(from);
	await start(env);

	// The source picker swaps directly; the camera has captured nothing yet.
	env.source.set("camera");
	env.video.source.set(undefined);
	env.audio.source.set(undefined);
	env.video.settled.set(false);
	env.audio.settled.set(false);
	await settle();
	expect(env.announcing.peek()).toBe(false);

	// Video alone, such as with a denied microphone, can't advertise a partial camera publication.
	env.video.source.set({});
	env.video.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(false);

	env.audio.source.set({});
	env.audio.settled.set(true);
	await settle();
	expect(env.announcing.peek()).toBe(true);
});
