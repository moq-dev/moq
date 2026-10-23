/**
 * Puts one subscription's frames in timestamp order for the audio decoder.
 *
 * Groups go out newest-first, so a subscription's head (the groups the publisher still holds) arrives
 * after the live edge it was served alongside. Chrome's AudioDecoder stamps every output from its
 * first input, so decoding the head after the live edge shifts the rest of the track, and the ring,
 * anchored at the first decoded frame, cannot place the head anyway.
 *
 * A buffered subscription holds its head and releases it sorted. The head is complete once a newer
 * group than the first arrives, or once the first group alone spans the floor: the ring could not
 * start playing before holding that much, so waiting for the head costs no startup latency. Any frame
 * still older than one already released is dropped, as is every reordered frame when not holding.
 */
export class Order<T extends { group: number; frame: { timestamp: number } }> {
	// The head, oldest first, while it is still arriving.
	#held: T[] | undefined;
	// The group the head started with, and its own oldest frame.
	#first?: { group: number; timestamp: number };
	// How far the first group may run before the head is released.
	#floor: number;
	// The newest frame released since the decoder was last reset.
	#newest?: number;

	/** Hold the head until it spans `floor` (microseconds), or decode each frame as it arrives. */
	constructor(hold: { floor: number } | undefined) {
		this.#held = hold ? [] : undefined;
		this.#floor = hold?.floor ?? 0;
	}

	/** Accept a frame, returning the frames ready to decode, oldest first. */
	push(read: T): T[] {
		const held = this.#held;
		if (!held) return this.#release([read]);

		const { group, frame } = read;
		this.#first ??= { group, timestamp: frame.timestamp };
		if (group > this.#first.group) return this.#release([...this.#take(), read]);

		held.splice(held.findLastIndex((other) => other.frame.timestamp <= frame.timestamp) + 1, 0, read);
		if (group === this.#first.group && frame.timestamp - this.#first.timestamp >= this.#floor) {
			return this.#release(this.#take());
		}
		return [];
	}

	/** Release the head as it stands: the track ended, or a gap or end cut it short. */
	flush(): T[] {
		return this.#release(this.#take());
	}

	/** Start the order over after the decoder resets, which restarts its output timestamps. */
	reset(): void {
		this.#newest = undefined;
	}

	#take(): T[] {
		const held = this.#held ?? [];
		this.#held = undefined;
		return held;
	}

	#release(reads: T[]): T[] {
		return reads.filter(({ frame }) => {
			if (this.#newest !== undefined && frame.timestamp < this.#newest) return false;
			this.#newest = frame.timestamp;
			return true;
		});
	}
}
