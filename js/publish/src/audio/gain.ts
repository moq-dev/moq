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

	/**
	 * Scale a frame, advancing the ramp across its samples.
	 *
	 * Returns a new frame rather than writing through: one capture feeds every rendition, and each
	 * has its own volume, so scaling in place would let one rendition's mute silence the others.
	 * At unity it returns the input untouched, which is the common case and copies nothing.
	 */
	apply(frame: AudioFrame, sampleRate: number): AudioFrame {
		// Unity already, and staying there: every sample would be multiplied by 1.
		if (this.#current === 1 && this.#target === 1) return frame;

		// How far the level may move per sample to cover the full range in FADE seconds.
		const step = 1 / (FADE * sampleRate);
		const samples = frame.channels[0]?.length ?? 0;
		const channels = frame.channels.map((channel) => new Float32Array(channel));

		for (let index = 0; index < samples; index++) {
			if (this.#current < this.#target) this.#current = Math.min(this.#target, this.#current + step);
			else if (this.#current > this.#target) this.#current = Math.max(this.#target, this.#current - step);

			if (this.#current === 1) continue;
			for (const channel of channels) channel[index] *= this.#current;
		}

		return { timestamp: frame.timestamp, channels };
	}
}
