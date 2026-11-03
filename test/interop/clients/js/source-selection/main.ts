/** Actual browser decoder probes against malformed and partial catalog metadata. @module */
import * as Catalog from "@moq/hang/catalog";
import { Audio, type Broadcast, Signals, Video } from "@moq/watch";

const good = Catalog.AudioConfigSchema.parse({
	codec: "opus",
	sampleRate: 48000,
	numberOfChannels: 2,
	container: { kind: "legacy" },
});
const bad = { ...good, codec: "" };
const catalog = new Signals.Signal<Catalog.Root>({
	audio: { renditions: { bad, good } },
	video: {
		renditions: {
			picture: Catalog.VideoConfigSchema.parse({
				codec: "vp8",
				codedWidth: 640,
				codedHeight: 360,
				container: { kind: "legacy" },
			}),
			bitrate: Catalog.VideoConfigSchema.parse({ codec: "vp8", bitrate: 500000, container: { kind: "legacy" } }),
		},
	},
});
const broadcast = { out: { catalog } } as unknown as Broadcast;
const audio = new Audio.Source({ broadcast, supported: Audio.Decoder.supported });
const video = new Video.Source({
	broadcast,
	supported: Video.Decoder.supported,
	target: { width: 640, height: 360, bitrate: 1000000 },
});
const effect = new Signals.Effect();
effect.cleanup(() => {
	audio.close();
	video.close();
});
effect.event(window, "pagehide", () => effect.close());
effect.run((effect) => {
	document.body.dataset.audio = effect.get(audio.out.track) ?? "";
	document.body.dataset.audioAvailable = Object.keys(effect.get(audio.out.available)).join(",");
	document.body.dataset.video = effect.get(video.out.track) ?? "";
	document.body.dataset.videoAvailable = String(Object.keys(effect.get(video.out.available)).length);
});
effect.event(document.querySelector("#bad") as HTMLButtonElement, "click", () =>
	catalog.update((c) => ({ ...c, audio: { renditions: { bad } } })),
);
effect.event(document.querySelector("#recover") as HTMLButtonElement, "click", () =>
	catalog.update((c) => ({ ...c, audio: { renditions: { recovered: good } } })),
);
