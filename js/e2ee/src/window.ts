import { DATAGRAM_WINDOW } from "./limits.ts";

/**
 * Datagram duplicate suppression: a 1024-bit sliding bitmask below the greatest opened sequence.
 *
 * Only successful opens are marked, so a forged datagram never burns a real one.
 * Sequences below the window are unknown, not duplicates: an operational gap.
 */
export class DatagramWindow {
	#highest?: number;
	#bits = new Uint32Array(DATAGRAM_WINDOW / 32);

	isDuplicate(sequence: number): boolean {
		const highest = this.#highest;
		if (highest === undefined) return false;
		return sequence <= highest && highest - sequence < DATAGRAM_WINDOW && this.#bit(sequence);
	}

	mark(sequence: number): void {
		const highest = this.#highest;
		if (highest === undefined) {
			this.#highest = sequence;
		} else if (sequence > highest) {
			// Slide forward, clearing the slots the new sequences will reuse.
			const last = Math.min(sequence, highest + DATAGRAM_WINDOW);
			for (let cleared = highest + 1; cleared <= last; cleared++) this.#set(cleared, false);
			this.#highest = sequence;
		} else if (highest - sequence >= DATAGRAM_WINDOW) {
			return;
		}
		this.#set(sequence, true);
	}

	#bit(sequence: number): boolean {
		const slot = sequence % DATAGRAM_WINDOW;
		return (this.#bits[slot >>> 5] & (1 << (slot & 31))) !== 0;
	}

	#set(sequence: number, value: boolean): void {
		const slot = sequence % DATAGRAM_WINDOW;
		const mask = 1 << (slot & 31);
		if (value) this.#bits[slot >>> 5] |= mask;
		else this.#bits[slot >>> 5] &= ~mask;
	}
}
