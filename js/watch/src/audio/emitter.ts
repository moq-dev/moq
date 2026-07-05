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
			const context = root.context;

			// Safari starts the AudioContext suspended and will NOT render a source->destination edge
			// that was wired while suspended, even after a later resume(). So build the graph only once
			// the context is actually running: the first gesture-driven resume (see decoder.ts) flips
			// this, and the edge is then wired live, exactly like the working mute->unmute path.
			const running = new Signal(context.state === "running");
			effect.event(context, "statechange", () => running.set(context.state === "running"));

			effect.run((inner) => {
				if (!inner.get(running)) return;

				// peek (not get) the volume: the fade effect below owns volume changes. Subscribing here
				// would rebuild the whole graph on every change and cut the fade short with a click.
				const gain = new GainNode(context, { gain: this.volume.peek() });
				root.connect(gain);
				inner.cleanup(() => {
					// The decoder can tear down its worklet first, dropping the root->gain edge; a
					// disconnect of an already-disconnected node throws InvalidAccessError. Swallow it:
					// this cleanup runs inside the signals dispose loop, where a throw would wedge the
					// effect and leave audio permanently silent.
					try {
						root.disconnect(gain);
					} catch {}
				});

				inner.set(this.#gain, gain);

				inner.run((leaf) => {
					// We only connect/disconnect when enabled to save power.
					// Otherwise the worklet keeps running in the background returning 0s.
					if (!leaf.get(this.source.enabled)) return;

					gain.connect(context.destination); // speakers
					leaf.cleanup(() => gain.disconnect());
				});
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
