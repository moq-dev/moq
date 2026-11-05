// The loudest comfort noise, in RMS: -60 dBFS. Audio that never falls quiet, such as music, would
// otherwise fill a pause with noise as loud as itself.
const MAX_LEVEL = 0.001;

/**
 * Fills a declared silence with white noise at the level of the quietest stretch heard before it, so
 * a muted talker's background continues instead of the line going dead. The publisher sends nothing
 * while muted, so the level is measured from the audio the listener played: the quietest 20 ms block
 * since playback last resumed is the closest thing to the talker's background, and a source that was
 * already silent there, such as a noise-suppressed microphone, gets none.
 */
export class ComfortNoise {
	readonly #block: number;
	// The block being measured: summed squares across channels, and the frames in it so far.
	#sum = 0;
	#frames = 0;
	// The mean square of the quietest whole block since playback last resumed.
	#quietest = Number.POSITIVE_INFINITY;
	// The RMS of the noise for the current silence.
	#level = 0;
	#filling = false;

	constructor(rate: number) {
		this.#block = Math.max(1, Math.round(rate / 50));
	}

	/** Measure the first `samples` of `output`, just played. */
	played(output: Float32Array[], samples: number): void {
		if (samples === 0) return;
		if (this.#filling) {
			this.#filling = false;
			this.#quietest = Number.POSITIVE_INFINITY;
			this.#sum = 0;
			this.#frames = 0;
		}
		for (let i = 0; i < samples; i++) {
			for (const channel of output) this.#sum += channel[i] * channel[i];
			if (++this.#frames < this.#block) continue;
			this.#quietest = Math.min(this.#quietest, this.#sum / (this.#block * output.length));
			this.#sum = 0;
			this.#frames = 0;
		}
	}

	/** Fill `output` from `from` onward with comfort noise. */
	fill(output: Float32Array[], from: number): void {
		if (!this.#filling) {
			this.#filling = true;
			// A silence after too little audio to measure keeps the previous level.
			if (Number.isFinite(this.#quietest)) this.#level = Math.min(Math.sqrt(this.#quietest), MAX_LEVEL);
		}
		if (this.#level === 0) return;
		// Uniform noise on [-a, a] has an RMS of a / sqrt(3).
		const amplitude = this.#level * Math.sqrt(3);
		for (const channel of output) {
			for (let i = from; i < channel.length; i++) channel[i] = (Math.random() * 2 - 1) * amplitude;
		}
	}
}
