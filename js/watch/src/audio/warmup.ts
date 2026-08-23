/** Tracks decoder callbacks discarded during initial playback setup. */
export class Warmup {
	#remaining: number;

	/** Create a warm-up window containing `callbacks` decoder callbacks. */
	constructor(callbacks: number) {
		this.#remaining = callbacks;
	}

	/** Consume one callback and report whether it belongs to the warm-up window. */
	drop(): boolean {
		if (this.#remaining === 0) return false;
		this.#remaining--;
		return true;
	}
}
