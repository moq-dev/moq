/**
 * Puts one subscription's frames in timestamp order for the audio decoder.
 *
 * Groups go out newest-first, so a subscription's head (the groups the publisher still holds) arrives
 * after the live edge it was served alongside. Chrome's AudioDecoder stamps every output from its
 * first input, so decoding the head after the live edge shifts the rest of the track, and the ring,
 * anchored at the first decoded frame, cannot place the head anyway.
 *
 * A buffered subscription holds its head and releases it sorted. The head is complete once a newer
 * group than the first arrives, or once the first group alone runs past the floor: the ring could not
 * start playing before holding that much, so waiting for the head costs no startup latency. It is
 * also released once it spans the subscription's max delay, since nothing older is served. After
 * that, and throughout when not holding, a frame older than one already decoded is not admitted.
 */
export class Order<T extends { group: number; frame: { timestamp: number } }> {
	// The current bounds in microseconds, while the head is still arriving.
	#hold: (() => { floor: number; maxDelay: number }) | undefined;
	// The head, in arrival order.
	#held: T[] = [];
	// The group the head started with, and its own first frame.
	#first?: { group: number; timestamp: number };
	// The oldest and newest held timestamps.
	#span?: { oldest: number; newest: number };
	// The newest frame admitted since the decoder was last reset.
	#newest?: number;

	/** Hold the head within the bounds `hold` reports, or pass each frame straight through. */
	constructor(hold: (() => { floor: number; maxDelay: number }) | undefined) {
		this.#hold = hold;
	}

	/** Accept a frame, returning the frames released for decoding, oldest first. */
	push(read: T): T[] {
		const hold = this.#hold?.();
		if (!hold) return [read];

		const { group, frame } = read;
		this.#first ??= { group, timestamp: frame.timestamp };
		if (group > this.#first.group) return [...this.flush(), read];

		this.#held.push(read);
		const oldest = Math.min(this.#span?.oldest ?? frame.timestamp, frame.timestamp);
		const newest = Math.max(this.#span?.newest ?? frame.timestamp, frame.timestamp);
		this.#span = { oldest, newest };

		const ran = group === this.#first.group && frame.timestamp - this.#first.timestamp > hold.floor;
		if (ran || newest - oldest >= hold.maxDelay) return this.flush();
		return [];
	}

	/** Release the head as it stands, ending the hold: the track ended, or a gap or end cut it short. */
	flush(): T[] {
		this.#hold = undefined;
		const held = this.#held;
		this.#held = [];
		// Stable, so frames sharing a timestamp keep their arrival order.
		return held.sort((a, b) => a.frame.timestamp - b.frame.timestamp);
	}

	/** Whether a released frame is still in order for the decoder, recording it if so. */
	admit(read: T): boolean {
		const { timestamp } = read.frame;
		if (this.#newest !== undefined && timestamp < this.#newest) return false;
		this.#newest = timestamp;
		return true;
	}

	/** Start the order over after the decoder resets, which restarts its output timestamps. */
	reset(): void {
		this.#newest = undefined;
	}
}
