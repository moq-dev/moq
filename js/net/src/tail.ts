import { type GetPromise, Signal } from "@moq/signals";
import { Milli } from "./time.ts";

/**
 * How long a subscriber waits for a group stream it cannot account for once the publisher
 * has ended the subscription.
 *
 * A group reset before its header arrived leaves no trace, and QUIC does not order streams,
 * so a stream opened before the end can still be in flight after it. This bounds the wait on
 * IETF, and on moq-lite when the subscription has no max age to bound it with.
 */
export const TAIL_GRACE_MS = Milli(1000);

// setTimeout truncates a longer delay to a signed 32-bit int and fires at once.
const MAX_TIMEOUT_MS = 2 ** 31 - 1;

/**
 * The group streams a subscription has received, so its end can wait for the ones still owed.
 *
 * A publisher ends a subscription before every group stream it opened has necessarily
 * arrived. This records which sequences are accounted for (a stream's header arrived, or
 * the publisher dropped them) and how many streams are still being read, and {@link settle}
 * waits on them.
 *
 * @internal
 */
export class Tail {
	// Disjoint, sorted, exclusive-end ranges of accounted sequences. A gap splits a range, so
	// this stays as small as the number of gaps rather than the number of groups.
	#accounted: [number, number][] = [];
	// Group streams whose header arrived, and those still being read.
	#streams = 0;
	#active = 0;
	#changed = new Signal(0);

	/** Group streams whose header arrived, whether they finished or were reset. */
	get streams(): number {
		return this.#streams;
	}

	/**
	 * Record a group stream whose header arrived. Returns the call that marks it read to
	 * its end, which is idempotent.
	 */
	open(sequence: number): () => void {
		this.#streams += 1;
		this.#active += 1;
		this.#account(sequence, sequence + 1);

		let closed = false;
		return () => {
			if (closed) return;
			closed = true;
			this.#active -= 1;
			this.#bump();
		};
	}

	/** Record sequences `[start, end)` as accounted for without a stream: dropped, or a datagram. */
	account(start: number, end: number): void {
		this.#account(start, end);
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: number, end: number): boolean {
		if (start >= end) return true;
		// Ranges are merged on insert, so one range covers the span or none does.
		return this.#accounted.some(([lo, hi]) => lo <= start && end <= hi);
	}

	/**
	 * Wait until every stream is read to its end and `complete()` holds, or `grace`
	 * milliseconds pass with nothing left being read, or `closed` settles.
	 *
	 * A stream still being read is always waited for: a group ends on its own stream's FIN
	 * or reset, never because its track ended. The grace only gives up on streams that never
	 * arrived.
	 */
	async settle(complete: () => boolean, grace: Milli, closed: GetPromise<unknown>): Promise<void> {
		let expired = false;
		const timer = setTimeout(
			() => {
				expired = true;
				this.#bump();
			},
			Math.min(grace, MAX_TIMEOUT_MS),
		);
		try {
			while (closed.peek() === undefined) {
				if (this.#active === 0 && (expired || complete())) return;
				await Signal.race(this.#changed, closed);
			}
		} finally {
			clearTimeout(timer);
		}
	}

	#account(start: number, end: number): void {
		if (start >= end) return;

		const merged: [number, number][] = [];
		let lo = start;
		let hi = end;
		let placed = false;
		for (const range of this.#accounted) {
			if (range[1] < lo) {
				merged.push(range);
			} else if (hi < range[0]) {
				if (!placed) merged.push([lo, hi]);
				placed = true;
				merged.push(range);
			} else {
				lo = Math.min(lo, range[0]);
				hi = Math.max(hi, range[1]);
			}
		}
		if (!placed) merged.push([lo, hi]);
		this.#accounted = merged;
		this.#bump();
	}

	#bump(): void {
		this.#changed.update((revision) => revision + 1);
	}
}
