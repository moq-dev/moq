import { expect, mock, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Bandwidth, Origin, Path } from "@moq/net";
import { Signal } from "@moq/signals";

// Bun does not run Vite's worklet loader; these tests never create an AudioContext.
mock.module("./audio/render-worklet.ts?worklet", () => ({ default: async () => "blob:fake-render" }));

const { Player } = await import("./player");
const Audio = await import("./audio");
const Video = await import("./video");

async function flush() {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

test("Player refuses the renamed reload and announced inputs", () => {
	expect(() => new Player({ reload: true } as never)).toThrow("`reload` was renamed to `follow`");
	expect(() => new Player({ announced: true } as never)).toThrow("`announced` was renamed to `follow`");
});

test.each(["table", "consumer", "structural"] as const)("Player borrows a %s origin", async (role) => {
	const producer = new Origin.Producer();
	const consumer = producer.consume();
	const table: Origin.Table = producer;
	const origins = {
		table,
		consumer,
		structural: {
			closed: consumer.closed,
			discovery: consumer.discovery,
			request: consumer.request.bind(consumer),
			follow: consumer.follow.bind(consumer),
			announced: consumer.announced.bind(consumer),
		},
	};
	const name = Path.from("live.hang");
	const player = new Player({ origin: origins[role], name, catalogFormat: "manual", catalog: {} });
	try {
		const active = player.broadcast.out.active.changed();
		const publication = producer.route(name);
		expect((await active)?.path).toBe(name);
		expect(player.in.origin.peek()).toBe(origins[role]);
		expect(player.broadcast.in.origin.peek()).toBe(origins[role]);

		player.close();
		await flush();
		expect(producer.closed.peek()).toBeUndefined();
		expect(publication.broadcast.closed.peek()).toBeUndefined();
	} finally {
		player.close();
		producer.close();
	}
});

test("Player owns the shared pipeline and feeds output policy into decoders", async () => {
	const enabled = new Signal(true);
	const paused = new Signal(false);
	const muted = new Signal(false);
	const player = new Player({ enabled, paused, audio: { muted }, video: { visible: "always" } });
	try {
		await flush();
		expect(player.video.source.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.audio.source.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.text.in.broadcast.peek()).toBe(player.broadcast);
		expect(player.video.sync).toBe(player.sync);
		expect(player.audio.sync).toBe(player.sync);
		expect(player.renderer.decoder).toBe(player.video);
		expect(player.emitter.source).toBe(player.audio);
		expect(player.video.in.enabled.peek()).toBe(true);
		expect(player.audio.in.enabled.peek()).toBe(true);
		expect(player.textRenderer.in.enabled.peek()).toBe(true);

		muted.set(true);
		await flush();
		expect(player.audio.in.enabled.peek()).toBe(false);
		expect(player.video.in.enabled.peek()).toBe(true);

		paused.set(true);
		await flush();
		expect(player.textRenderer.in.enabled.peek()).toBe(false);
		// A paused player keeps one video frame as a poster.
		expect(player.video.in.enabled.peek()).toBe(true);

		enabled.set(false);
		await flush();
		expect(player.broadcast.in.enabled.peek()).toBe(false);
		expect(player.video.in.enabled.peek()).toBe(false);
		expect(player.audio.in.enabled.peek()).toBe(false);
		expect(player.textRenderer.in.enabled.peek()).toBe(false);
	} finally {
		player.close();
	}
});

test("Player claims its viewer only while it downloads, and an override replaces the catalog's ceiling", async () => {
	const viewers = new Bandwidth.Viewers();
	const enabled = new Signal(true);
	const muted = new Signal(false);
	const max = new Signal<number | undefined>(1_500_000);
	const player = new Player({ enabled, audio: { muted }, video: { visible: "never" }, viewer: { viewers, max } });
	try {
		await flush();
		expect(viewers.target.peek()).toBe(1_500_000);

		// The override, too, claims only while something downloads.
		muted.set(true);
		await flush();
		expect(viewers.target.peek()).toBe(0);
		muted.set(false);
		await flush();
		expect(viewers.target.peek()).toBe(1_500_000);

		enabled.set(false);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		enabled.set(true);
		max.set(0);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		// Without an override, a player with no catalog yet claims nothing.
		max.set(undefined);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		max.set(2_000_000);
		await flush();
		expect(viewers.target.peek()).toBe(2_000_000);
	} finally {
		player.close();
	}
	await flush();
	expect(viewers.target.peek()).toBe(0);
	viewers.close();
});

test("Player selects video from its viewer grant less audio, moving up as the grant rises", async () => {
	// Bun has no WebCodecs or Web Audio: accept every rendition so selection runs, and park the
	// audio graph, which builds for any selected rendition, before it needs a worklet.
	const supported = { video: Video.Decoder.supported, audio: Audio.Decoder.supported };
	Video.Decoder.supported = async () => true;
	Audio.Decoder.supported = async () => true;
	const context = Object.getOwnPropertyDescriptor(globalThis, "AudioContext");
	globalThis.AudioContext = class extends EventTarget {
		state = "running";
		audioWorklet = { addModule: () => new Promise<void>(() => {}) };
		suspend = async () => {};
		resume = async () => {};
		close = async () => {};
	} as unknown as typeof AudioContext;

	const estimate = new Signal<number | undefined>(undefined);
	const viewers = new Bandwidth.Viewers(estimate);
	const visible = new Signal<"always" | "never">("always");
	const video = (width: number, height: number, bitrate: number) =>
		Catalog.VideoConfigSchema.parse({
			codec: "avc1.640028",
			container: { kind: "legacy" },
			codedWidth: width,
			codedHeight: height,
			bitrate,
		});
	const catalog = {
		video: {
			renditions: {
				"360p": video(640, 360, 800_000),
				"1080p": video(1920, 1080, 4_000_000),
			},
		},
		audio: {
			renditions: {
				audio: Catalog.AudioConfigSchema.parse({
					codec: "opus",
					sampleRate: 48000,
					numberOfChannels: 2,
					container: { kind: "legacy" },
					bitrate: 100_000,
				}),
			},
		},
	};
	const player = new Player({
		name: Path.from("live"),
		catalogFormat: "manual",
		catalog,
		video: { visible },
		viewer: { viewers },
	});
	try {
		await flush();
		// The 1080p rendition over the selection margin, plus audio.
		expect(viewers.target.peek()).toBe(5_100_000);

		// An app-limited estimate funds only the small rendition.
		estimate.set(1_200_000);
		await flush();
		expect(player.video.source.out.track.peek()).toBe("360p");

		// Short of the target by one bit, the audio's share leaves too little for 1080p.
		estimate.set(5_099_999);
		await flush();
		expect(player.video.source.out.track.peek()).toBe("360p");

		// A probe that reaches the target moves the viewer up.
		estimate.set(5_100_000);
		await flush();
		expect(player.video.source.out.track.peek()).toBe("1080p");

		// Hidden video claims only audio, which says nothing about video: the selection holds.
		visible.set("never");
		await flush();
		expect(viewers.target.peek()).toBe(100_000);
		expect(player.video.source.out.track.peek()).toBe("1080p");
	} finally {
		player.close();
		viewers.close();
		Video.Decoder.supported = supported.video;
		Audio.Decoder.supported = supported.audio;
		if (context) Object.defineProperty(globalThis, "AudioContext", context);
		else delete (globalThis as { AudioContext?: unknown }).AudioContext;
	}
});
