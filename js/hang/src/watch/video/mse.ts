import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import * as Container from "../../container";
import { type BufferedRanges, timeRangesToArray } from "../backend";
import type { Broadcast } from "../broadcast";
import { Sync } from "../sync";
import type { Backend, Stats, Target } from "./backend";

export type MseProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	mediaSource?: MediaSource | Signal<MediaSource | undefined>;
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	target?: Target | Signal<Target | undefined>;
	sync?: Sync;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Mse implements Backend {
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

	#buffered = new Signal<BufferedRanges>([]);
	readonly buffered: Getter<BufferedRanges> = this.#buffered;

	// The selected rendition as a separate signal so we don't resubscribe until it changes.
	#selected = new Signal<{ track: string; mime: string; config: Catalog.VideoConfig } | undefined>(undefined);

	sync: Sync;

	signals = new Effect();

	constructor(props?: MseProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.mediaSource = Signal.from(props?.mediaSource);
		this.target = Signal.from(props?.target);
		this.element = Signal.from(props?.element);

		this.sync = props?.sync ?? new Sync();

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

			// Support both CMAF and legacy containers
			if (target?.name && track !== target.name) continue;

			effect.set(this.#selected, { track, mime, config });
			effect.set(this.sync.video, config.delay as Moq.Time.Milli | undefined);

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
		selected: { track: string; mime: string; config: Catalog.VideoConfig },
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		if (selected.config.container.kind !== "cmaf") return;

		const data = active.subscribe(selected.track, Catalog.PRIORITY.video);
		effect.cleanup(() => data.close());

		effect.spawn(async () => {
			// Generate init segment from catalog config (uses track_id from container)
			const initSegment = Container.Cmaf.createVideoInitSegment(selected.config);
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
		selected: { track: string; mime: string; config: Catalog.VideoConfig },
		sourceBuffer: SourceBuffer,
		element: HTMLMediaElement,
	): void {
		const data = active.subscribe(selected.track, Catalog.PRIORITY.video);
		effect.cleanup(() => data.close());

		// Create consumer that reorders groups/frames up to the provided latency.
		// Legacy container uses microsecond timescale implicitly.
		const consumer = new Container.Legacy.Consumer(data, {
			latency: this.sync.latency,
		});
		effect.cleanup(() => consumer.close());

		effect.spawn(async () => {
			// Generate init segment from catalog config (timescale = 1,000,000 = microseconds)
			const initSegment = Container.Cmaf.createVideoInitSegment(selected.config);
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
				const segment = Container.Cmaf.encodeDataSegment({
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

	close(): void {
		this.signals.close();
	}
}
