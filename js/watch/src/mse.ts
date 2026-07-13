import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Sync } from "./sync";

type MuxerInput = {
	element: Getter<HTMLMediaElement | undefined>;
	paused: Getter<boolean>;
};

type MuxerOutput = {
	mediaSource: Signal<MediaSource | undefined>;
};

/**
 * MSE-based video source for CMAF/fMP4 fragments.
 * Uses Media Source Extensions to handle complete moof+mdat fragments.
 */
export class Muxer {
	readonly in: Readonlys<MuxerInput>;
	sync: Sync;

	readonly #out: MuxerOutput = {
		mediaSource: new Signal<MediaSource | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(sync: Sync, props?: Inputs<MuxerInput>) {
		this.in = {
			element: getter(props?.element),
			paused: getter(props?.paused ?? false),
		};
		this.sync = sync;

		this.#signals.run(this.#runMediaSource.bind(this));
		this.#signals.run(this.#runSkip.bind(this));
		this.#signals.run(this.#runTrim.bind(this));
		this.#signals.run(this.#runPaused.bind(this));
		this.#signals.run(this.#runSync.bind(this));
	}

	#runMediaSource(effect: Effect): void {
		const element = effect.get(this.in.element);
		if (!element) return;

		const mediaSource = new MediaSource();

		element.src = URL.createObjectURL(mediaSource);
		effect.cleanup(() => URL.revokeObjectURL(element.src));

		effect.event(
			mediaSource,
			"sourceopen",
			() => {
				effect.set(this.#out.mediaSource, mediaSource);
			},
			{ once: true },
		);

		effect.event(mediaSource, "error", (e) => {
			console.error("[MSE] MediaSource error event:", e);
		});
	}

	#runSkip(effect: Effect): void {
		const element = effect.get(this.in.element);
		if (!element) return;

		// Don't skip when paused, otherwise we'll keep jerking forward.
		const paused = effect.get(this.in.paused);
		if (paused) return;

		// Use the computed latency (catalog jitter + user jitter)
		// Convert to seconds since DOM APIs use seconds
		const latency = Time.Milli.toSecond(effect.get(this.sync.out.buffer));

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
		const element = effect.get(this.in.element);
		if (!element) return;

		const media = effect.get(this.out.mediaSource);
		if (!media) return;

		// Periodically clean up old buffered data.
		effect.interval(() => {
			for (const sourceBuffer of media.sourceBuffers) {
				// Skip a buffer mid-update; the next tick (1s later) catches it.
				if (sourceBuffer.updating) continue;

				// Keep at least 10 seconds of buffered data to avoid removing I-frames.
				if (element.currentTime > 10) {
					sourceBuffer.remove(0, element.currentTime - 10);
				}
			}
		}, 1000);
	}

	#runPaused(effect: Effect): void {
		const element = effect.get(this.in.element);
		if (!element) return;

		const paused = effect.get(this.in.paused);
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
		const element = effect.get(this.in.element);
		if (!element) return;

		// Don't seek when paused, otherwise we'll keep jerking around.
		const paused = effect.get(this.in.paused);
		if (paused) return;

		const reference = effect.get(this.sync.out.reference);
		if (reference === undefined) return;

		const latency = effect.get(this.sync.out.buffer);

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
