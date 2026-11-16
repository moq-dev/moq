import { DATAGRAM_WINDOW } from "./limits.ts";

const WINDOW = BigInt(DATAGRAM_WINDOW);

/**
 * Datagram duplicate suppression: a 1024-bit sliding bitmask below the greatest opened sequence.
 *
 * Only successful opens are marked, so a forged datagram never burns a real one.
 * Sequences below the window are unknown, not duplicates: an operational gap.
 */
export class DatagramWindow {
	#highest?: bigint;
	#bits = new Uint32Array(DATAGRAM_WINDOW / 32);

	isDuplicate(sequence: bigint): boolean {
		const highest = this.#highest;
		if (highest === undefined) return false;
		return sequence <= highest && highest - sequence < WINDOW && this.#bit(sequence);
	}

	mark(sequence: bigint): void {
		const highest = this.#highest;
		if (highest === undefined) {
			this.#highest = sequence;
		} else if (sequence > highest) {
			// Slide forward, clearing the slots the new sequences will reuse.
			const last = sequence < highest + WINDOW ? sequence : highest + WINDOW;
			for (let cleared = highest + 1n; cleared <= last; cleared++) this.#set(cleared, false);
			this.#highest = sequence;
		} else if (highest - sequence >= WINDOW) {
			return;
		}
		this.#set(sequence, true);
	}

	#bit(sequence: bigint): boolean {
		const slot = Number(sequence % WINDOW);
		return (this.#bits[slot >>> 5] & (1 << (slot & 31))) !== 0;
	}

	#set(sequence: bigint, value: boolean): void {
		const slot = Number(sequence % WINDOW);
		const mask = 1 << (slot & 31);
		if (value) this.#bits[slot >>> 5] |= mask;
		else this.#bits[slot >>> 5] &= ~mask;
	}
}
