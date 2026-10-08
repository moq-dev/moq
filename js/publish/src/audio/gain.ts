import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";

/**
 * Scales PCM toward a target level, ramping across samples rather than jumping to it.
 *
 * One implementation for every source. The capture graph used to ride a Web Audio gain node while
 * decoded samples were scaled by hand, so muting followed a different curve depending on where the
 * audio came from.
 */
export class Gain {
	#current: number;
	#target: number;

	// The ramp toward #target, in ms until the next frame converts it to samples at that frame's rate.
	#fade: Time.Milli = Time.Milli.zero;
	#remaining: number | undefined = 0;

	/** Start at `initial`, avoiding a ramp from unity before the first frame. */
	constructor(initial = 1) {
		this.#current = initial;
		this.#target = initial;
	}

	/**
	 * Ramp toward `value`, where 1 is unity and 0 is silence, reaching it `fade` after the next frame
	 * starts. Every change takes the whole `fade` whatever its size, so a mute is silent on time.
	 * A `fade` of 0 steps at once. Repeating the current target leaves a ramp in progress alone.
	 */
	set(value: number, fade: Time.Milli): void {
		if (value === this.#target) return;
		this.#target = value;
		this.#fade = fade;
		this.#remaining = undefined;
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

		this.#remaining ??= Math.round(Time.Milli.toSecond(this.#fade) * sampleRate);

		const samples = frame.channels[0]?.length ?? 0;
		const channels = frame.channels.map((channel) => new Float32Array(channel));

		for (let index = 0; index < samples; index++) {
			// Spread what is left evenly over the samples left, landing on the target exactly.
			if (this.#remaining <= 1) this.#current = this.#target;
			else this.#current += (this.#target - this.#current) / this.#remaining;
			if (this.#remaining > 0) this.#remaining--;

			if (this.#current === 1) continue;
			for (const channel of channels) channel[index] *= this.#current;
		}

		return { timestamp: frame.timestamp, channels };
	}
}
