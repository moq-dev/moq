import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Decoder } from "./decoder";

const MIN_GAIN = 0.001;
const FADE_TIME = 0.2;

type EmitterInput = {
	volume: Getter<number>;

	// Silences the audio and stops the download. Muted samples aren't worth the bandwidth,
	// and the decoder keeps the AudioContext warm so unmuting is still instant.
	muted: Getter<boolean>;

	// Pauses playback, which also stops the download.
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
			paused: getter(props?.paused ?? false),
		};

		// Only download while playing audible audio. Pausing or muting stops it.
		this.#signals.run((effect) => {
			const enabled = !effect.get(this.input.paused) && !effect.get(this.input.muted);
			this.#output.enabled.set(enabled);
		});

		this.#signals.run((effect) => {
			const root = effect.get(this.source.output.root);
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
				const gain = new GainNode(context, { gain: this.input.volume.peek() });
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
					if (!leaf.get(this.#output.enabled)) return;

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
