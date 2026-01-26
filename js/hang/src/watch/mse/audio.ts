import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import * as Frame from "../../frame";
import * as Mp4 from "../../mp4";
import { Latency } from "../../util/latency";
import type { Backend, Stats, Target } from "../audio/backend";
import { type BufferedRanges, timeRangesToArray } from "../backend";
import type { Broadcast } from "../broadcast";

export type AudioProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	mediaSource?: MediaSource | Signal<MediaSource | undefined>;

	// Additional buffer in milliseconds on top of the catalog's minBuffer.
	buffer?: Moq.Time.Milli | Signal<Moq.Time.Milli>;
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
	buffer: Signal<Moq.Time.Milli>;

	#catalog = new Signal<Catalog.Audio | undefined>(undefined);
	readonly catalog: Getter<Catalog.Audio | undefined> = this.#catalog;

	#rendition = new Signal<string | undefined>(undefined);
	readonly rendition: Signal<string | undefined> = this.#rendition;

	#stats = new Signal<Stats | undefined>(undefined);
	readonly stats: Signal<Stats | undefined> = this.#stats;

	#config = new Signal<Catalog.AudioConfig | undefined>(undefined);
	readonly config: Signal<Catalog.AudioConfig | undefined> = this.#config;

	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	#selected = new Signal<{ track: string; mime: string; config: Catalog.AudioConfig } | undefined>(undefined);

	#latency: Latency;
	readonly latency: Getter<Moq.Time.Milli>;

	#signals = new Effect();

	constructor(props?: AudioProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.element = Signal.from(props?.element);
		this.mediaSource = Signal.from(props?.mediaSource);

		this.buffer = Signal.from(props?.buffer ?? (100 as Moq.Time.Milli));
		this.volume = Signal.from(props?.volume ?? 0.5);
		this.muted = Signal.from(props?.muted ?? false);
		this.target = Signal.from(props?.target);

		this.#latency = new Latency({
			buffer: this.buffer,
			config: this.config,
		});
		this.latency = this.#latency.combined;

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

			// Support both CMAF and legacy containers
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

		effect.event(sourceBuffer, "error", (e) => {
			console.error("[MSE] SourceBuffer error:", e);
		});

		effect.event(sourceBuffer, "updateend", () => {
			this.#buffered.set(timeRangesToArray(sourceBuffer.buffered));
		});

		if (selected.config.container.kind === "cmaf") {
			this.#runCmafMedia(effect, active, selected, sourceBuffer, element);
		} else {
			this.#runLegacyMedia(effect, active, selected, sourceBuffer, element);
		}
	}

	async #appendBuffer(sourceBuffer: SourceBuffer, buffer: Uint8Array): Promise<void> {
		while (sourceBuffer.updating) {
			await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
		}
		sourceBuffer.appendBuffer(buffer as BufferSource);
		while (sourceBuffer.updating) {
			await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
		}
	}

	#runCmafMedia(
		effect: Effect,
		active: Moq.Broadcast,
		selected: { track: string; mime: string; config: Catalog.AudioConfig },
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		if (selected.config.container.kind !== "cmaf") return;

		const data = active.subscribe(selected.track, Catalog.PRIORITY.audio);
		effect.cleanup(() => data.close());

		effect.spawn(async () => {
			// Generate init segment from catalog config (uses track_id from container)
			const initSegment = Mp4.createAudioInitSegment(selected.config);
			await this.#appendBuffer(sourceBuffer, initSegment);

			for (;;) {
				// TODO: Use Frame.Consumer for CMAF so we can support higher latencies.
				// It requires extracting the timestamp from the frame payload.
				const frame = await data.readFrame();
				if (!frame) return;

				await this.#appendBuffer(sourceBuffer, frame);

				// Seek to the start of the buffer if we're behind it (for startup).
				if (element.buffered.length > 0 && element.currentTime < element.buffered.start(0)) {
					element.currentTime = element.buffered.start(0);
				}
			}
		});
	}

	#runLegacyMedia(
		effect: Effect,
		active: Moq.Broadcast,
		selected: { track: string; mime: string; config: Catalog.AudioConfig },
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		const data = active.subscribe(selected.track, Catalog.PRIORITY.audio);
		effect.cleanup(() => data.close());

		// Create consumer that reorders groups/frames up to the provided latency.
		// Legacy container uses microsecond timescale implicitly.
		const consumer = new Frame.Consumer(data, {
			latency: this.#latency.combined,
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			// Generate init segment from catalog config (timescale = 1,000,000 = microseconds)
			const initSegment = Mp4.createAudioInitSegment(selected.config);
			await this.#appendBuffer(sourceBuffer, initSegment);

			let sequence = 1;
			let duration: Moq.Time.Micro | undefined;

			// Buffer one frame so we can compute accurate duration from the next frame's timestamp
			let pending = await consumer.decode();
			if (!pending) return;

			for (;;) {
				const next = await consumer.decode();

				// Compute duration from next frame's timestamp, or use last known duration if stream ended
				if (next) {
					duration = (next.timestamp - pending.timestamp) as Moq.Time.Micro;
				}

				// Wrap raw frame in moof+mdat
				const segment = Mp4.encodeDataSegment({
					data: pending.data,
					timestamp: pending.timestamp,
					duration: duration ?? 0, // Default to 0 duration if there's literally one frame then stream FIN.
					keyframe: pending.keyframe,
					sequence: sequence++,
				});

				await this.#appendBuffer(sourceBuffer, segment);

				// Seek to the start of the buffer if we're behind it (for startup).
				if (element.buffered.length > 0 && element.currentTime < element.buffered.start(0)) {
					element.currentTime = element.buffered.start(0);
				}

				if (!next) return;
				pending = next;
			}
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
