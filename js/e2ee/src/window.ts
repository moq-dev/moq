import { DEFAULT_DATAGRAM_WINDOW } from "./constants.ts";
import { Failure } from "./error.ts";

/**
 * Bounded at-most-once window for grouped frames.
 *
 * Retains the current group's frame indices plus the previous group, as the profile
 * recommends. Identities outside the window are not treated as duplicates: a relay
 * may still replay them.
 */
export class GroupWindow {
	#current?: { sequence: number; frames: Set<number> };
	#previous?: { sequence: number; frames: Set<number> };

	/**
	 * Record `(group, frame)` or throw {@link Failure} `duplicate` if it is still retained.
	 * A newer group rotates the window; an older group than `previous` is outside it.
	 */
	claim(group: number, frame: number): void {
		if (this.#current && group === this.#current.sequence) {
			if (this.#current.frames.has(frame)) throw new Failure("duplicate");
			this.#current.frames.add(frame);
			return;
		}
		if (this.#previous && group === this.#previous.sequence) {
			if (this.#previous.frames.has(frame)) throw new Failure("duplicate");
			this.#previous.frames.add(frame);
			return;
		}
		if (this.#current && group > this.#current.sequence) {
			this.#previous = this.#current;
			this.#current = { sequence: group, frames: new Set([frame]) };
			return;
		}
		if (!this.#current) {
			this.#current = { sequence: group, frames: new Set([frame]) };
			return;
		}
		// Older than the retained pair: outside the window, not a duplicate.
		if (!this.#previous || group > this.#previous.sequence) {
			this.#previous = { sequence: group, frames: new Set([frame]) };
		}
	}
}

/**
 * Bounded at-most-once window for datagram sequences.
 *
 * Remembers the most recently claimed sequences, up to {@link DEFAULT_DATAGRAM_WINDOW}.
 * Evicted identities are outside the window and may be opened again.
 */
export class DatagramWindow {
	readonly size: number;
	#seen = new Set<number>();

	/** Create a window that retains `size` sequences (default 1024). */
	constructor(size = DEFAULT_DATAGRAM_WINDOW) {
		if (!Number.isInteger(size) || size < 1) {
			throw new RangeError(`datagram window must be a positive integer: ${size}`);
		}
		this.size = size;
	}

	/** Record `sequence` or throw {@link Failure} `duplicate` if it is still retained. */
	claim(sequence: number): void {
		if (this.#seen.has(sequence)) throw new Failure("duplicate");
		this.#seen.add(sequence);
		if (this.#seen.size > this.size) {
			const oldest = this.#seen.values().next().value;
			if (oldest !== undefined) this.#seen.delete(oldest);
		}
	}
}
