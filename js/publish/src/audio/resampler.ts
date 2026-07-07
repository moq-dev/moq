/**
 * Continuous linear resampler for a stream of PCM chunks, used to feed Opus a canonical 48 kHz stream
 * when the capture context runs at a non-canonical hardware rate (Safari pins ~44.1 kHz and ignores
 * `AudioContext({ sampleRate: 48000 })`).
 *
 * @module
 */

/**
 * Continuous linear resampler for a stream of PCM chunks (one rate -> another). Carries the sub-sample
 * phase and the previous chunk's last sample across chunks, so there is NO per-chunk discontinuity. A
 * per-chunk-independent resample would inject a slope kink every ~3 ms that the Opus encoder then bakes
 * in as an audible buzz.
 */
export class StreamResampler {
	readonly #ratio: number; // input samples advanced per output sample
	#pos = 0; // source position (input-sample units, relative to the current chunk start) of the next output
	#prev: number[] | undefined; // last input sample per channel from the previous chunk
	#emitted = 0; // total output samples emitted across all chunks (drives the output-clock timestamp)

	/** Build a resampler from `inputRate` Hz to `outputRate` Hz. */
	constructor(inputRate: number, outputRate: number) {
		this.#ratio = inputRate / outputRate;
	}

	/**
	 * Total output samples produced so far. The caller stamps each resampled AudioData with a timestamp
	 * derived from this counter (anchor + emitted / outputRate) so timestamps advance on the SAME clock as
	 * the sample counts. Stamping with the raw capture-clock timestamp instead makes consecutive Opus
	 * frames land a fraction of a sample off in the watcher's timestamp-indexed ring buffer, which
	 * zero-fills or overwrites a sample every frame and crackles.
	 */
	get emitted(): number {
		return this.#emitted;
	}

	/** Resample one chunk of planar PCM; returns the output chunk (possibly empty if nothing is ready yet). */
	resample(channels: Float32Array[]): Float32Array[] {
		const n = channels[0]?.length ?? 0;
		if (n === 0) return channels.map(() => new Float32Array(0));

		const ratio = this.#ratio;
		const numCh = channels.length;

		// How many output samples fall within reach this chunk (their bracketing input samples exist).
		let count = 0;
		for (let p = this.#pos; p <= n - 1; p += ratio) count++;

		const out = channels.map(() => new Float32Array(count));
		let p = this.#pos;
		for (let j = 0; j < count; j++, p += ratio) {
			const i0 = Math.floor(p);
			const frac = p - i0;
			for (let c = 0; c < numCh; c++) {
				const ch = channels[c];
				// i0 is >= -1 (the carried phase never falls further back than the previous chunk's tail).
				const s0 = i0 < 0 ? (this.#prev?.[c] ?? ch[0]) : ch[i0];
				const s1 = ch[Math.min(i0 + 1, n - 1)];
				out[c][j] = s0 * (1 - frac) + s1 * frac;
			}
		}

		// Shift the next output position into the next chunk's coordinate; remember the boundary sample.
		this.#pos = p - n;
		this.#prev = channels.map((ch) => ch[n - 1]);
		this.#emitted += count;
		return out;
	}
}
