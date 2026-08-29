import { Decoder as Flate } from "@moq/flate";

import type { Op } from "./encoder.ts";

/** Options for a {@link Decoder}, and so for the {@link Consumer} wrapping one. */
export interface ConsumerConfig {
	/** Read frames written with `ProducerConfig.compression` on. Defaults to `false`. */
	compression?: boolean;
}

/**
 * One change to the window, as the consumer sees it.
 *
 * Every index is reported exactly once: `push` when a record first reaches this consumer, `pop`
 * when it leaves the window, and `skip` when it existed but was dropped before this consumer ever
 * saw it.
 */
export type Event<T> = { push: { index: number; value: T } } | { pop: number } | { skip: number };

/**
 * Reconstructs window events from frame payloads.
 *
 * The track-free core of {@link Consumer}. It tracks indices, not contents: it knows where the
 * window starts and how far it has delivered, which is all it needs to turn a reset into the
 * pushes, pops, and skips the reader has not already been told about.
 *
 * Group rolls are invisible here on purpose. A reset restates the window, and this decoder emits
 * only what is new, so a reader sees one continuous stream of edits no matter how often the
 * publisher rolled for compression's sake.
 */
export class Decoder<T> {
	#compress: boolean;
	#flate?: Flate;

	// Absolute index of the window's front, once a reset has positioned us.
	#front = 0;
	#len = 0;
	// Next index to deliver, or undefined before the first reset. A fresh consumer adopts the first
	// reset's offset rather than skipping everything that came before it.
	#delivered?: number;

	#events: Event<T>[] = [];

	constructor(config: ConsumerConfig = {}) {
		this.#compress = config.compression ?? false;
	}

	/**
	 * Start a cold DEFLATE window, for a reader that has just moved to a new group.
	 *
	 * Only the compression state resets. The index cursor deliberately survives: it is what lets the
	 * next group's reset report just the records this reader has not seen.
	 */
	reset(): void {
		this.#flate = undefined;
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#front;
	}

	/** Take the next event produced by the frames decoded so far. */
	next(): Event<T> | undefined {
		return this.#events.shift();
	}

	/** Decode one frame, queueing the events it implies. */
	decode(payload: Uint8Array): void {
		if (this.#compress) this.#flate ??= new Flate();
		const bytes = this.#flate ? this.#flate.frame(payload) : payload;
		const op = JSON.parse(new TextDecoder().decode(bytes)) as Op<T>;

		if ("reset" in op) {
			this.#applyReset(op.reset.offset, op.reset.records);
		} else if ("push" in op) {
			this.#applyPush(op.push);
		} else if ("pop" in op) {
			this.#applyPop(op.pop);
		} else {
			throw new Error("unrecognized window op");
		}
	}

	/** The window is exactly these records. Report what this reader missed, then what is new. */
	#applyReset(offset: number, records: T[]): void {
		const end = offset + records.length;
		let delivered = this.#delivered;

		if (delivered === undefined) {
			// First position: adopt the publisher's offset rather than skipping all of history.
			delivered = offset;
		} else {
			// Records that left the window while we were away. Those we had delivered are pops; those we
			// never saw are skips. The ranges are disjoint and together cover everything that left, so
			// every index is still reported exactly once.
			for (let index = this.#front; index < Math.min(delivered, offset); index++) {
				this.#events.push({ pop: index });
			}
			for (let index = delivered; index < offset; index++) {
				this.#events.push({ skip: index });
			}
		}

		// Deliver only the tail this reader has not seen; a reset that merely restates what it holds
		// yields nothing at all.
		for (let index = Math.max(delivered, offset); index < end; index++) {
			this.#events.push({ push: { index, value: records[index - offset] as T } });
		}

		this.#front = offset;
		this.#len = end - offset;
		this.#delivered = Math.max(delivered, end);
	}

	/** One record joined the back. */
	#applyPush(value: T): void {
		// A group always opens with a reset, so a push before one means we started mid-group.
		if (this.#delivered === undefined) throw new Error("window op before reset");

		const index = this.#front + this.#len;
		this.#len += 1;

		if (index >= this.#delivered) {
			this.#events.push({ push: { index, value } });
			this.#delivered = index + 1;
		}
	}

	/** Records left the front. */
	#applyPop(count: number): void {
		if (this.#delivered === undefined) throw new Error("window op before reset");
		if (count > this.#len) {
			throw new Error(`pop of ${count} exceeds the ${this.#len} record(s) in the window`);
		}

		for (let index = this.#front; index < this.#front + count; index++) {
			// Within a group every frame is seen, so these were delivered; the skip arm only matters for
			// a window that was already ahead of this reader.
			this.#events.push(index < this.#delivered ? { pop: index } : { skip: index });
		}

		this.#front += count;
		this.#len -= count;
		this.#delivered = Math.max(this.#delivered, this.#front);
	}
}
