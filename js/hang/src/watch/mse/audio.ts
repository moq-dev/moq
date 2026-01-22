import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import type { Backend, Stats, Target } from "../audio/backend";
import type { Broadcast } from "../broadcast";

export type AudioProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	mediaSource?: MediaSource | Signal<MediaSource | undefined>;

	volume?: number | Signal<number>;
	muted?: boolean | Signal<boolean>;
	target?: Target | Signal<Target | undefined>;
};

export class Audio implements Backend {
	broadcast: Signal<Broadcast | undefined>;
	element: Signal<HTMLMediaElement | undefined>;
	mediaSource: Signal<MediaSource | undefined>;

	volume: Signal<number>;
	muted: Signal<boolean>;
	target: Signal<Target | undefined>;

	#catalog = new Signal<Catalog.Audio | undefined>(undefined);
	readonly catalog: Getter<Catalog.Audio | undefined> = this.#catalog;

	#rendition = new Signal<string | undefined>(undefined);
	readonly rendition: Signal<string | undefined> = this.#rendition;

	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Signal<Stats | undefined> = this.#stats;

	#config = new Signal<Catalog.AudioConfig | undefined>(undefined);
	readonly config: Signal<Catalog.AudioConfig | undefined> = this.#config;

	#selected = new Signal<{ track: string; mime: string; config: Catalog.AudioConfig } | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: AudioProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.element = Signal.from(props?.element);
		this.mediaSource = Signal.from(props?.mediaSource);

		this.volume = Signal.from(props?.volume ?? 0.5);
		this.muted = Signal.from(props?.muted ?? false);
		this.target = Signal.from(props?.target);

		this.#signals.effect(this.#runCatalog.bind(this));
		this.#signals.effect(this.#runSelected.bind(this));
		this.#signals.effect(this.#runMedia.bind(this));
		this.#signals.effect(this.#runVolume.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const active = effect.get(broadcast.active);
		if (!active) return;

		const catalog = effect.get(broadcast.catalog)?.audio;
		if (!catalog) return;

		effect.set(this.#catalog, catalog);
	}

	#runSelected(effect: Effect): void {
		const catalog = effect.get(this.#catalog);
		if (!catalog) return;

		const target = effect.get(this.target);

		for (const [track, config] of Object.entries(catalog.renditions)) {
			const mime = `audio/mp4; codecs="${config.codec}"`;
			if (!MediaSource.isTypeSupported(mime)) continue;

			// TODO support legacy
			if (config.container.kind !== "cmaf") continue;
			if (target?.name && track !== target.name) continue;

			effect.set(this.#selected, { track, mime, config });
			return;
		}

		console.warn(`[MSE] No supported audio rendition found:`, catalog.renditions);
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

		const selected = effect.get(this.#selected);
		if (!selected) return;

		const sourceBuffer = mediaSource.addSourceBuffer(selected.mime);
		effect.cleanup(() => {
			mediaSource.removeSourceBuffer(sourceBuffer);
			sourceBuffer.abort();
		});

		let init: Moq.Track | undefined;
		if (selected.config.container.kind === "cmaf") {
			init = active.subscribe(selected.config.container.init_track.name, Catalog.PRIORITY.audio);
			effect.cleanup(() => init?.close());
		}

		const data = active.subscribe(selected.track, Catalog.PRIORITY.audio);
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

				// Wait until we're ready to append the next frame.
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

	#runVolume(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const volume = effect.get(this.volume);
		const muted = effect.get(this.muted);

		if (muted && !element.muted) {
			element.muted = true;
		} else if (!muted && element.muted) {
			element.muted = false;
		}

		if (volume !== element.volume) {
			element.volume = volume;
		}

		effect.event(element, "volumechange", () => {
			this.volume.set(element.volume);
		});
	}

	close(): void {
		this.#signals.close();
	}
}
