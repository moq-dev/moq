import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Sync } from "./sync";

export type MuxerProps = {
	element?: HTMLMediaElement | Signal<HTMLMediaElement | undefined>;
	paused?: boolean | Signal<boolean>;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Muxer {
	element: Signal<HTMLMediaElement | undefined>;

	paused: Signal<boolean>;

	#sync: Sync;

	#mediaSource = new Signal<MediaSource | undefined>(undefined);
	readonly mediaSource: Getter<MediaSource | undefined> = this.#mediaSource;

	#signals = new Effect();

	constructor(sync: Sync, props?: MuxerProps) {
		this.element = Signal.from(props?.element);
		this.paused = Signal.from(props?.paused ?? false);
		this.#sync = sync;

		this.#signals.effect(this.#runMediaSource.bind(this));
		this.#signals.effect(this.#runSkip.bind(this));
		this.#signals.effect(this.#runTrim.bind(this));
		this.#signals.effect(this.#runPaused.bind(this));
		this.#signals.effect(this.#runSync.bind(this));
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
		// Convert to seconds since DOM APIs use seconds
		const latency = Time.Milli.toSecond(effect.get(this.#sync.latency));

		effect.interval(() => {
			// Skip over gaps based on the effective latency.
			const buffered = element.buffered;
			if (buffered.length === 0) return;

			const last = buffered.end(buffered.length - 1);
			const target = last - latency;
			const seek = target - element.currentTime;

			// Seek forward if we're too far behind, or backward if we're too far ahead (>100ms)
			if (seek > 0.1 || seek < -0.1) {
				console.warn("seeking", seek > 0 ? "forward" : "backward", Math.abs(seek).toFixed(3), "seconds");
				element.currentTime = target;
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

	#runPaused(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		const paused = effect.get(this.paused);
		if (paused && !element.paused) {
			element.pause();
		} else if (!paused && element.paused) {
			element.play().catch((e) => {
				console.error("[MSE] MediaElement play error:", e);
			});
		}
	}

	// Seek to the target position based on the reference and latency.
	#runSync(effect: Effect): void {
		const element = effect.get(this.element);
		if (!element) return;

		// Don't seek when paused, otherwise we'll keep jerking around.
		const paused = effect.get(this.paused);
		if (paused) return;

		const reference = effect.get(this.#sync.reference);
		if (reference === undefined) return;

		const latency = effect.get(this.#sync.latency);

		// Compute the target currentTime based on reference and latency.
		// reference = performance.now() - frameTimestamp (in ms) when we received the earliest frame
		// So the target media timestamp (in ms) at time `now` is: now - reference - latency
		const target = Time.Milli.sub(Time.Milli.sub(Time.Milli.now(), reference), latency);

		// Seek to the target position.
		element.currentTime = Time.Milli.toSecond(target);
	}

	close(): void {
		this.#signals.close();
	}
}
