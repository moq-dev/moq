/** Tracks decoder callbacks discarded during initial playback setup. */
export class Warmup {
	#remaining: number;
	#ungated: number;

	/** Create a warm-up window containing `callbacks` decoder callbacks. */
	constructor(callbacks: number) {
		this.#remaining = callbacks;
		this.#ungated = callbacks;
	}

	/** Consume one callback and report whether it belongs to the warm-up window. */
	drop(): boolean {
		if (this.#remaining === 0) return false;
		this.#remaining--;
		return true;
	}

	/**
	 * Consume one decoder input and report whether its output falls in the warm-up window. That
	 * output never reaches the ring, so the input skips the ring's backpressure: gating it would
	 * spend the ring's refill on audio that is discarded, and the ring would never anchor.
	 */
	ungated(): boolean {
		if (this.#ungated === 0) return false;
		this.#ungated--;
		return true;
	}
}
