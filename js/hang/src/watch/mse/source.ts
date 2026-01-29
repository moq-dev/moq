import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Backend } from "../backend";
import type { Broadcast } from "../broadcast";
import { Sync } from "../sync";
import { Audio, type AudioProps } from "./audio";
import { Video, type VideoProps } from "./video";

export type SourceProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	// Additional buffer in milliseconds on top of the catalog's minBuffer.
	buffer?: Moq.Time.Milli | Signal<Moq.Time.Milli>;
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	paused?: boolean | Signal<boolean>;

	video?: VideoProps;
	audio?: AudioProps;

	sync?: Sync;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Source implements Backend {
	broadcast: Signal<Broadcast | undefined>;

	#mediaSource = new Signal<MediaSource | undefined>(undefined);

	element: Signal<HTMLMediaElement | undefined>;
	buffer: Signal<Moq.Time.Milli>;
	paused: Signal<boolean>;

	video: Video;
	audio: Audio;
	sync: Sync;

	#buffering = new Signal<boolean>(false);
	readonly buffering: Getter<boolean> = this.#buffering;

	#timestamp = new Signal<number>(0);
	readonly timestamp: Getter<number> = this.#timestamp;

	#signals = new Effect();

	constructor(props?: SourceProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.buffer = Signal.from(props?.buffer ?? (100 as Moq.Time.Milli));
		this.element = Signal.from(props?.element);
		this.paused = Signal.from(props?.paused ?? false);
		this.sync = props?.sync ?? new Sync();

		this.video = new Video({
			broadcast: this.broadcast,
			element: this.element,
			mediaSource: this.#mediaSource,
			sync: this.sync,
			...props?.video,
		});
		this.audio = new Audio({
			broadcast: this.broadcast,
			element: this.element,
			mediaSource: this.#mediaSource,
			sync: this.sync,
			...props?.audio,
		});

		this.#signals.effect(this.#runMediaSource.bind(this));
		this.#signals.effect(this.#runSkip.bind(this));
		this.#signals.effect(this.#runTrim.bind(this));
		this.#signals.effect(this.#runBuffering.bind(this));
		this.#signals.effect(this.#runPaused.bind(this));
		this.#signals.effect(this.#runTimestamp.bind(this));
	}

	#runMediaSource(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const mediaSource = new MediaSource();

		element.src = URL.createObjectURL(mediaSource);
		effect.cleanup(() => URL.revokeObjectURL(element.src));

		effect.event(
			mediaSource,
			"sourceopen",
			() => {
				effect.set(this.#mediaSource, mediaSource);
			},
			{ once: true },
		);

		effect.event(mediaSource, "error", (e) => {
			console.error("[MSE] MediaSource error event:", e);
		});
	}

	#runSkip(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		// Don't skip when paused, otherwise we'll keep jerking forward.
		const paused = effect.get(this.paused);
		if (paused) return;

		// Use the computed latency (catalog minBuffer + user buffer)
		const latency = effect.get(this.sync.latency);

		effect.interval(() => {
			// Skip over gaps based on the effective latency.
			const buffered = element.buffered;
			if (buffered.length === 0) return;

			const last = buffered.end(buffered.length - 1);
			const diff = last - element.currentTime;

			// We seek an extra 100ms because seeking isn't free/instant.
			if (diff > latency && diff > 0.1) {
				console.warn("skipping ahead", diff, "seconds");
				element.currentTime += diff + 0.1;
			}
		}, 100);
	}

	#runTrim(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const mediaSource = effect.get(this.#mediaSource);
		if (!mediaSource) return;

		// Periodically clean up old buffered data.
		effect.interval(async () => {
			for (const sourceBuffer of mediaSource.sourceBuffers) {
				while (sourceBuffer.updating) {
					await new Promise((resolve) => sourceBuffer.addEventListener("updateend", resolve, { once: true }));
				}

				// Keep at least 10 seconds of buffered data to avoid removing I-frames.
				if (element.currentTime > 10) {
					sourceBuffer.remove(0, element.currentTime - 10);
				}
			}
		}, 1000);
	}

	#runBuffering(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const update = () => {
			this.#buffering.set(element.readyState <= HTMLMediaElement.HAVE_CURRENT_DATA);
		};

		// TODO Are these the correct events to use?
		effect.event(element, "waiting", update);
		effect.event(element, "playing", update);
		effect.event(element, "seeking", update);
	}

	#runPaused(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const paused = effect.get(this.paused);
		if (paused && !element.paused) {
			element.pause();
		} else if (!paused && element.paused) {
			element.play().catch((e) => {
				console.error("[MSE] MediaElement play error:", e);
				this.paused.set(false);
			});
		}
	}

	#runTimestamp(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		// Use requestVideoFrameCallback if available (frame-accurate)
		if ("requestVideoFrameCallback" in element) {
			const video = element as HTMLVideoElement;
			let handle: number;
			const onFrame = () => {
				this.#timestamp.set(video.currentTime);
				handle = video.requestVideoFrameCallback(onFrame);
			};
			handle = video.requestVideoFrameCallback(onFrame);
			effect.cleanup(() => video.cancelVideoFrameCallback(handle));
		} else {
			// Fallback to timeupdate event
			effect.event(element, "timeupdate", () => {
				this.#timestamp.set(element.currentTime);
			});
		}
	}

	close(): void {
		this.#signals.close();
	}
}
