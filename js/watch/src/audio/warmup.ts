/** Tracks decoder callbacks discarded after each codec configuration. */
export class Warmup {
	#frames: number;
	#remaining: number;

	/** Create a warm-up window containing `frames` decoder callbacks. */
	constructor(frames: number) {
		this.#frames = frames;
		this.#remaining = frames;
	}

	/** Restart the warm-up window for a fresh codec epoch. */
	reset(): void {
		this.#remaining = this.#frames;
	}

	/** Consume one callback and report whether it belongs to the warm-up window. */
	drop(): boolean {
		if (this.#remaining === 0) return false;
		this.#remaining--;
		return true;
	}
}
