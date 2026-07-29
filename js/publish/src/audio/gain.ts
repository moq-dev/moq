import type { AudioFrame } from "./capture";

// Ramp over this long so a mute or a volume change doesn't click.
const FADE = 0.2; // seconds

/**
 * Scales PCM toward a target level, ramping across samples rather than jumping to it.
 *
 * One implementation for every source. The capture graph used to ride a Web Audio gain node while
 * decoded samples were scaled by hand, so muting followed a different curve depending on where the
 * audio came from.
 */
export class Gain {
	#current = 1;
	#target = 1;

	/** Set the level to ramp toward, where 1 is unity and 0 is silence. */
	set(value: number): void {
		this.#target = value;
	}

	/** Scale a frame in place, advancing the ramp across its samples. */
	apply(frame: AudioFrame, sampleRate: number): void {
		// Unity already, and staying there: every sample would be multiplied by 1.
		if (this.#current === 1 && this.#target === 1) return;

		// How far the level may move per sample to cover the full range in FADE seconds.
		const step = 1 / (FADE * sampleRate);
		const samples = frame.channels[0]?.length ?? 0;

		for (let index = 0; index < samples; index++) {
			if (this.#current < this.#target) this.#current = Math.min(this.#target, this.#current + step);
			else if (this.#current > this.#target) this.#current = Math.max(this.#target, this.#current - step);

			if (this.#current === 1) continue;
			for (const channel of frame.channels) channel[index] *= this.#current;
		}
	}
}
