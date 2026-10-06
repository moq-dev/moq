import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

const FADE = Time.Milli(200);

export type EmitterInput = {
	volume: Getter<number>;

	// Silences the audio and stops the download. Muted samples aren't worth the bandwidth,
	// and the decoder keeps the AudioContext warm so unmuting is still instant.
	muted: Getter<boolean>;

	// Pauses playback, which also stops the download.
	paused: Getter<boolean>;
};

/** Constructor properties for {@link Emitter}. */
export type EmitterProps = Inputs<EmitterInput> & {
	/** Decoder supplying PCM. */
	source: Decoder;

	/** How long a volume change ramps for. Defaults to 200 ms; 0 steps at once. */
	fade?: Time.Milli | Signal<Time.Milli>;
};

type EmitterOutput = {
	// Whether audio should be downloaded. Wired into the decoder's `enabled` input by the owner.
	enabled: Signal<boolean>;
};

// A helper that emits audio directly to the speakers.
export class Emitter {
	readonly source: Decoder;

	readonly in: Readonlys<EmitterInput>;

	/** How long a volume change ramps for. 0 steps at once; a negative or non-finite fade throws and leaves the volume as it was. */
	fade: Signal<Time.Milli>;

	readonly #out: EmitterOutput = {
		enabled: new Signal<boolean>(false),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	// The gain node used to adjust the volume.
	#gain = new Signal<GainNode | undefined>(undefined);

	constructor(props: EmitterProps) {
		this.source = props.source;
		this.in = {
			volume: getter(props?.volume ?? 0.5),
			muted: getter(props?.muted ?? false),
			paused: getter(props?.paused ?? false),
		};
		this.fade = Signal.from(props.fade ?? FADE);

		// Only download while playing audible audio. Pausing or muting stops it.
		this.#signals.run((effect) => {
			const enabled = !effect.get(this.in.paused) && !effect.get(this.in.muted);
			this.#out.enabled.set(enabled);
		});

		this.#signals.run((effect) => {
			const root = effect.get(this.source.out.root);
			if (!root) return;

			// Seed the level without subscribing: rebuilding the node on a volume change would jump
			// straight to the new level and skip the fade below.
			const gain = new GainNode(root.context, { gain: this.in.volume.peek() });
			root.connect(gain);

			effect.set(this.#gain, gain);

			effect.run((inner) => {
				// We only connect/disconnect when enabled to save power.
				// Otherwise the worklet keeps running in the background returning 0s.
				const enabled = inner.get(this.#out.enabled);
				if (!enabled) return;

				gain.connect(root.context.destination); // speakers
				inner.cleanup(() => gain.disconnect());
			});
		});

		this.#signals.run((effect) => {
			const gain = effect.get(this.#gain);
			if (!gain) return;

			// On a change, hold wherever the ramp in progress has reached, so the next one starts from
			// there. Cancelling alone would snap back to the level the ramp started from.
			effect.cleanup(() => {
				const now = gain.context.currentTime;
				const level = gain.gain.value;
				gain.gain.cancelScheduledValues(now);
				gain.gain.setValueAtTime(level, now);
			});

			const fade = effect.get(this.fade);
			if (!Number.isFinite(fade) || fade < 0)
				throw new Error(`audio fade must be a finite, non-negative number of ms: ${fade}`);

			// Linear, like the publisher's gain: an exponential ramp can't start from or reach silence.
			const volume = effect.get(this.in.volume);
			const now = gain.context.currentTime;
			if (fade === 0) gain.gain.setValueAtTime(volume, now);
			else gain.gain.linearRampToValueAtTime(volume, now + Time.Milli.toSecond(fade));
		});
	}

	close() {
		this.#signals.close();
	}
}
