import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import type { Broadcast } from "../broadcast";
import type { Backend, Stats, Target } from "../video/backend";

export type VideoProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	mediaSource?: MediaSource | Signal<MediaSource | undefined>;
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;

	target?: Target | Signal<Target | undefined>;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Video implements Backend {
	broadcast: Signal<Broadcast | undefined>;
	element: Signal<HTMLMediaElement | undefined>;
	mediaSource: Signal<MediaSource | undefined>;

	// TODO Modify #select to use this signal.
	target: Signal<Target | undefined>;

	#catalog = new Signal<Catalog.Video | undefined>(undefined);
	readonly catalog: Getter<Catalog.Video | undefined> = this.#catalog;

	#rendition = new Signal<string | undefined>(undefined);
	readonly rendition: Getter<string | undefined> = this.#rendition;

	// TODO implement stats
	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Getter<Stats | undefined> = this.#stats;

	#config = new Signal<Catalog.VideoConfig | undefined>(undefined);
	readonly config: Signal<Catalog.VideoConfig | undefined> = this.#config;

	// The selected rendition as a separate signal so we don't resubscribe until it changes.
	#selected = new Signal<{ track: string; mime: string; config: Catalog.VideoConfig } | undefined>(undefined);

	signals = new Effect();

	constructor(props?: VideoProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.mediaSource = Signal.from(props?.mediaSource);
		this.target = Signal.from(props?.target);
		this.element = Signal.from(props?.element);

		this.signals.effect(this.#runCatalog.bind(this));
		this.signals.effect(this.#runSelected.bind(this));
		this.signals.effect(this.#runMedia.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.catalog)?.video;
		if (!catalog) return;

		effect.set(this.#catalog, catalog);
	}

	#runSelected(effect: Effect): void {
		const catalog = effect.get(this.#catalog);
		if (!catalog) return;

		const target = effect.get(this.target);

		for (const [track, config] of Object.entries(catalog.renditions)) {
			const mime = `video/mp4; codecs="${config.codec}"`;
			if (!MediaSource.isTypeSupported(mime)) continue;

			// TODO support legacy
			if (config.container.kind !== "cmaf") continue;
			if (target?.name && track !== target.name) continue;

			effect.set(this.#selected, { track, mime, config });
			return;
		}

		console.warn(`[MSE] No supported video rendition found:`, catalog.renditions);
	}

	#runMedia(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const mediaSource = effect.get(this.mediaSource);
		if (!mediaSource) return;

		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const active = effect.get(broadcast.active);
		if (!active) return;

		// TODO Don't do a hard effect reload when this doesn't change the outcome.
		const selected = effect.get(this.#selected);
		if (!selected) return;

		const sourceBuffer = mediaSource.addSourceBuffer(selected.mime);
		effect.cleanup(() => {
			mediaSource.removeSourceBuffer(sourceBuffer);
			sourceBuffer.abort();
		});

		// If this is CMAF, we also need to subscribe to the init track.
		let init: Moq.Track | undefined;
		if (selected.config.container.kind === "cmaf") {
			init = active.subscribe(selected.config.container.init_track.name, Catalog.PRIORITY.video);
			effect.cleanup(() => init?.close());
		}

		const data = active.subscribe(selected.track, Catalog.PRIORITY.video);
		effect.cleanup(() => data.close());

		effect.spawn(async () => {
			if (init) {
				const frame = await init.readFrame();
				if (!frame) throw new Error("no init frame");

				sourceBuffer.appendBuffer(frame as BufferSource);
			}

			for (;;) {
				// TODO: Use Frame.Consumer for CMAF so we can support higher latencies.
				// It requires extracting the timestamp from the frame payload.
				const frame = await data.readFrame();
				if (!frame) return;

				// Wait until we can append the next frame.
				while (sourceBuffer.updating) {
					await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
				}

				sourceBuffer.appendBuffer(frame as BufferSource);

				// Wait until until that append is complete before continuing.
				while (sourceBuffer.updating) {
					await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
				}

				// Seek to the start of the buffer if we're behind it (for startup).
				if (element.buffered.length > 0 && element.currentTime < element.buffered.start(0)) {
					element.currentTime = element.buffered.start(0);
				}
			}
		});

		effect.event(sourceBuffer, "error", (e) => {
			console.error("[MSE] SourceBuffer error:", e);
		});
	}

	close(): void {
		this.signals.close();
	}
}
