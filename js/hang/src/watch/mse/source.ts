import * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import * as Frame from "../../frame";

export type SourceProps = {
	catalog: Catalog.Source | Signal<Catalog.Source | undefined>,
	latency?: Moq.Time.Milli | Signal<Moq.Time.Milli>;
	element?: HTMLVideoElement | Signal<HTMLVideoElement | undefined>;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Source {
	catalog: Signal<Catalog.Source | undefined>;

	#mediaSource = new Signal<MediaSource | undefined>(undefined);

	element: Signal<HTMLVideoElement | undefined>;

	latency: Signal<Moq.Time.Milli>;
	display = new Signal<{ width: number; height: number } | undefined>(undefined);
	flip = new Signal<boolean | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: SourceProps) {
		this.catalog = Signal.from(props?.catalog);
		this.latency = Signal.from(props?.latency ?? (100 as Moq.Time.Milli));
		this.element = Signal.from(props?.element);

		this.#signals.effect(this.#runMediaSource.bind(this));
		this.#signals.effect(this.#runVideo.bind(this));
		this.#signals.effect(this.#runAudio.bind(this));
	}

	#runMediaSource(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const mediaSource = new MediaSource();

		element.src = URL.createObjectURL(mediaSource);
		effect.cleanup(() => URL.revokeObjectURL(element.src));

		effect.set(this.#mediaSource, mediaSource);

		effect.event(element, "sourceopen", () => {
			effect.set(this.#mediaSource, mediaSource);
		}, { once: true });

		effect.event(element, "error", (e) => {
			console.error("[MSE] MediaSource error event:", e);
		});
	}

	#runVideo(effect: Effect): void {
		const mediaSource = effect.get(this.#mediaSource);
		if (!mediaSource) return;

		const catalog = effect.get(this.catalog);
		if (!catalog) return;

		const broadcast = effect.get(catalog.broadcast);
		if (!broadcast) return;

		const video = effect.get(catalog.parsed)?.video;
		if (!video) return;

		const rendition = this.#selectRendition("video", video.renditions);
		if (!rendition) return;

		const sourceBuffer = mediaSource.addSourceBuffer(rendition.mime);
		effect.cleanup(() => {
			mediaSource.removeSourceBuffer(sourceBuffer);
			sourceBuffer.abort();
		});

		const sub = broadcast.subscribe(rendition.track, Catalog.PRIORITY.video);
		console.log(`[MSE] Subscribing to video track: ${rendition.track}`);
		effect.cleanup(() => sub.close());

		this.#startSource(effect, sourceBuffer, sub);
	}

	#runAudio(effect: Effect): void {
		const catalog = effect.get(this.catalog);
		if (!catalog) return;

		const broadcast = effect.get(catalog.broadcast);
		if (!broadcast) return;

		const mediaSource = effect.get(this.#mediaSource);
		if (!mediaSource) return;

		const audio = effect.get(catalog.parsed)?.audio;
		if (!audio) return;

		const rendition = this.#selectRendition("audio", audio.renditions);
		if (!rendition) return;

		const sourceBuffer = mediaSource.addSourceBuffer(rendition.mime);
		effect.cleanup(() => {
			mediaSource.removeSourceBuffer(sourceBuffer);
			sourceBuffer.abort();
		});

		const sub = broadcast.subscribe(rendition.track, Catalog.PRIORITY.audio);
		console.log(`[MSE] Subscribing to audio track: ${rendition.track}`);
		effect.cleanup(() => sub.close());

		this.#startSource(effect, sourceBuffer, sub);
	}

	#selectRendition(type: "video" | "audio", renditions: Record<string, Catalog.VideoConfig | Catalog.AudioConfig>): { track: string, mime: string } | undefined {
		for (const [track, config] of Object.entries(renditions)) {
			const mime = `${type}/mp4; codecs="${config.codec}"`;
			if (!MediaSource.isTypeSupported(mime)) continue;

			return { track, mime };
		}

		console.warn(`[MSE] No supported ${type} rendition found:`, renditions);
		return undefined;
	}

	#startSource(effect: Effect, sourceBuffer: SourceBuffer, track: Moq.Track): void {
		const consumer = new Frame.Consumer(track, {
			latency: this.latency,
			container: "cmaf",
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			for (;;) {
				const frame = await consumer.decode();
				if (!frame) return;

				if (sourceBuffer.updating) {
					await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
				}

				sourceBuffer.appendBuffer(frame.data as BufferSource);
			}
		});

		effect.event(sourceBuffer, "error", (e) => {
			console.error("[MSE] SourceBuffer error:", e);
		});
	}

	close(): void {
		this.#signals.close();
	}
}
