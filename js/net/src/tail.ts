import { type Milli, Nano, now, sleep } from "@moq/clock";
import { type GetPromise, Signal } from "@moq/signals";
import { GRACE, Ledger } from "./ledger.ts";

/** How a {@link Tail} measures its grace. */
export interface TailOptions {
	/** How long to wait for a group that cannot be accounted for. Read each time it is needed. */
	grace?: () => Milli;
}

/**
 * The group streams a subscription has received, so its end can wait for the ones still owed.
 *
 * A publisher ends a subscription before every group stream it opened has necessarily
 * arrived. This records which sequences are accounted for (a stream's header arrived, or
 * the publisher dropped them) and how many streams are still being read, and {@link settle}
 * waits on them.
 *
 * A lost datagram is not owed: it leaves a hole that waits out the grace like a stream reset
 * before its header.
 *
 * @internal
 */
export class Tail {
	// The sequences accounted for, whose gaps fold once the grace stops waiting for them.
	#ledger = new Ledger();
	// Group streams whose header arrived, and those still being read.
	#streams = 0;
	#active = 0;
	#changed = new Signal(0);
	#grace: () => Milli;

	constructor(options: TailOptions = {}) {
		this.#grace = options.grace ?? (() => GRACE);
	}

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

	/**
	 * Hold the end open for something being read that is not one of the counted streams,
	 * such as a joining FETCH's head, or let go of a counted stream while it waits on one.
	 * Returns the call that lets go, which is idempotent.
	 */
	hold(): () => void {
		this.#active += 1;
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.#active -= 1;
			this.#bump();
		};
	}

	/**
	 * Stop holding the end open for a counted stream while it waits on something that holds
	 * it instead. Returns the call that holds it again, which must come before the stream's
	 * own close.
	 */
	suspend(): () => void {
		this.#active -= 1;
		this.#bump();
		let resumed = false;
		return () => {
			if (resumed) return;
			resumed = true;
			this.#active += 1;
		};
	}

	/** Re-check what {@link settle} waits on, after something outside the ledger changed. */
	touch(): void {
		this.#bump();
	}

	/** Record sequences `[start, end)` as accounted for without a stream: dropped, or a datagram. */
	account(start: number, end: number): void {
		this.#account(start, end);
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: number, end: number): void {
		this.#ledger.demand(start, end, now());
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: number, end: number): boolean {
		return this.#ledger.covers(start, end);
	}

	/**
	 * Wait until every stream is read to its end and `complete()` holds, or the grace passes
	 * with nothing left being read, or `closed` settles.
	 *
	 * A stream still being read is always waited for: a group ends on its own stream's FIN
	 * or reset, never because its track ended. The grace only gives up on streams that never
	 * arrived.
	 */
	async settle(complete: () => boolean, closed: GetPromise<unknown>): Promise<void> {
		// A gap that aged past the grace since the last insert is no longer waited for either.
		this.#ledger.expire(now(), this.#grace());

		let expired = false;
		const timer = new AbortController();
		void sleep(Nano.fromMilli(this.#grace()), { signal: timer.signal }).then(
			() => {
				expired = true;
				this.#bump();
			},
			() => {},
		);
		try {
			while (closed.peek() === undefined) {
				if (this.#active === 0 && (expired || complete())) return;
				await Signal.race(this.#changed, closed);
			}
		} finally {
			timer.abort();
		}
	}

	#account(start: number, end: number): void {
		if (start >= end) return;
		const at = now();
		this.#ledger.account(start, end, at);
		this.#ledger.expire(at, this.#grace());
		this.#bump();
	}

	#bump(): void {
		this.#changed.update((revision) => revision + 1);
	}
}
