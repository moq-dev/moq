import { Effect, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

const MIN_GAIN = 0.001;
const FADE_TIME = 0.2;

export type EmitterProps = {
	volume?: number | Signal<number>;
	muted?: boolean | Signal<boolean>;
	paused?: boolean | Signal<boolean>;
};

// A helper that emits audio directly to the speakers.
export class Emitter {
	source: Decoder;
	volume: Signal<number>;
	muted: Signal<boolean>;

	// Similar to muted, but controls whether we download audio at all.
	// That way we can be "muted" but also download audio for visualizations.
	paused: Signal<boolean>;

	#signals = new Effect();

	// The volume to use when unmuted.
	#unmuteVolume = 0.5;

	// The gain node used to adjust the volume.
	#gain = new Signal<GainNode | undefined>(undefined);

	constructor(source: Decoder, props?: EmitterProps) {
		this.source = source;
		this.volume = Signal.from(props?.volume ?? 0.5);
		this.muted = Signal.from(props?.muted ?? false);
		this.paused = Signal.from(props?.paused ?? props?.muted ?? false);

		// Set the volume to 0 when muted.
		this.#signals.run((effect) => {
			const muted = effect.get(this.muted);
			if (muted) {
				this.#unmuteVolume = this.volume.peek() || 0.5;
				this.volume.set(0);
			} else {
				this.volume.set(this.#unmuteVolume);
			}
		});

		this.#signals.run((effect) => {
			const enabled = !effect.get(this.paused) && !effect.get(this.muted);
			this.source.enabled.set(enabled);
		});

		// Set unmute when the volume is non-zero.
		this.#signals.run((effect) => {
			const volume = effect.get(this.volume);
			this.muted.set(volume === 0);
		});

		this.#signals.run((effect) => {
			const root = effect.get(this.source.root);
			if (!root) return;

			const gain = new GainNode(root.context, { gain: effect.get(this.volume) });
			root.connect(gain);

			effect.set(this.#gain, gain);

			effect.run(() => {
				// We only connect/disconnect when enabled to save power.
				// Otherwise the worklet keeps running in the background returning 0s.
				const enabled = effect.get(this.source.enabled);
				if (!enabled) return;

				gain.connect(root.context.destination); // speakers
				effect.cleanup(() => gain.disconnect());
			});
		});

		this.#signals.run((effect) => {
			const gain = effect.get(this.#gain);
			if (!gain) return;

			// Cancel any scheduled transitions on change.
			effect.cleanup(() => gain.gain.cancelScheduledValues(gain.context.currentTime));

			const volume = effect.get(this.volume);
			if (volume < MIN_GAIN) {
				gain.gain.exponentialRampToValueAtTime(MIN_GAIN, gain.context.currentTime + FADE_TIME);
				gain.gain.setValueAtTime(0, gain.context.currentTime + FADE_TIME + 0.01);
			} else {
				gain.gain.exponentialRampToValueAtTime(volume, gain.context.currentTime + FADE_TIME);
			}
		});
	}

	close() {
		this.#signals.close();
	}
}
