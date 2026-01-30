import type { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Sync } from "./sync";

export type MuxerProps = {
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	paused?: boolean | Signal<boolean>;
	jitter?: Time.Milli | Signal<Time.Milli>;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Muxer {
	element: Signal<HTMLMediaElement | undefined>;

	paused: Signal<boolean>;
	jitter: Signal<Time.Milli>;

	#sync: Sync;

	#mediaSource = new Signal<MediaSource | undefined>(undefined);
	readonly mediaSource: Getter<MediaSource | undefined> = this.#mediaSource;

	#buffering = new Signal<boolean>(false);
	readonly buffering: Getter<boolean> = this.#buffering;

	#timestamp = new Signal<number>(0);
	readonly timestamp: Getter<number> = this.#timestamp;

	#signals = new Effect();

	constructor(sync: Sync, props?: MuxerProps) {
		this.element = Signal.from(props?.element);
		this.paused = Signal.from(props?.paused ?? false);
		this.jitter = Signal.from(props?.jitter ?? (100 as Time.Milli));
		this.#sync = sync;

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

		// Use the computed latency (catalog jitter + user jitter)
		const latency = effect.get(this.#sync.latency);

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

		const media = effect.get(this.mediaSource);
		if (!media) return;

		// Periodically clean up old buffered data.
		effect.interval(async () => {
			for (const sourceBuffer of media.sourceBuffers) {
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
