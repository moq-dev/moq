import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

const MIN_GAIN = 0.001;
const FADE_TIME = 0.2;

type EmitterInput = {
	volume: Getter<number>;
	muted: Getter<boolean>;

	// Similar to muted, but controls whether we download audio at all.
	// That way we can be "muted" but also download audio for visualizations.
	paused: Getter<boolean>;
};

type EmitterOutput = {
	// Whether audio should be downloaded. Wired into the decoder's `enabled` input by the owner.
	enabled: Signal<boolean>;
};

// A helper that emits audio directly to the speakers.
export class Emitter {
	source: Decoder;

	readonly input: Readonlys<EmitterInput>;

	readonly #output: EmitterOutput = {
		enabled: new Signal<boolean>(false),
	};
	readonly output = readonlys(this.#output);

	#signals = new Effect();

	// The gain node used to adjust the volume.
	#gain = new Signal<GainNode | undefined>(undefined);

	constructor(source: Decoder, props?: Inputs<EmitterInput>) {
		this.source = source;
		this.input = {
			volume: getter(props?.volume ?? 0.5),
			muted: getter(props?.muted ?? false),
			paused: getter(props?.paused ?? props?.muted ?? false),
		};

		this.#signals.run((effect) => {
			const enabled = !effect.get(this.input.paused) && !effect.get(this.input.muted);
			this.#output.enabled.set(enabled);
		});

		this.#signals.run((effect) => {
			const root = effect.get(this.source.output.root);
			if (!root) return;

			const gain = new GainNode(root.context, { gain: effect.get(this.input.volume) });
			root.connect(gain);

			effect.set(this.#gain, gain);

			effect.run((inner) => {
				// We only connect/disconnect when enabled to save power.
				// Otherwise the worklet keeps running in the background returning 0s.
				const enabled = inner.get(this.#output.enabled);
				if (!enabled) return;

				gain.connect(root.context.destination); // speakers
				inner.cleanup(() => gain.disconnect());
			});
		});

		this.#signals.run((effect) => {
			const gain = effect.get(this.#gain);
			if (!gain) return;

			// Cancel any scheduled transitions on change.
			effect.cleanup(() => gain.gain.cancelScheduledValues(gain.context.currentTime));

			const volume = effect.get(this.input.volume);
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
