import { type Duration, now, sleep } from "@moq/clock";
import { type GetPromise, Signal } from "@moq/signals";
import { READER_RUN_OVERHEAD, type Reader } from "./cache.ts";
import { insertRun, type Run, runGaps } from "./internal.ts";
import { GRACE, Ledger } from "./ledger.ts";
import { max, min } from "./util/sequence.ts";

/** How a {@link Tail} measures its grace. */
export interface TailOptions {
	/** How long to wait for a group that cannot be accounted for. Read each time it is needed. */
	grace?: () => Duration;
	/** Bills the bytes the tail keeps to the subscription's copy's account, as a reader's record. */
	bill?: Reader;
}

/** A group stream a {@link Tail} counts, from its header until it is read to its end. */
export interface Counted {
	/** Mark it read to its end. Idempotent. */
	done(): void;
	/**
	 * The publisher accounted for its group another way, such as a drop, so {@link Tail.settle}
	 * waits for it at most the grace. Idempotent.
	 */
	excuse(): void;
	/** Hold the end again: the drop that excused it no longer stands, as an update reopens a stale one. */
	restore(): void;
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
	// Group streams whose header arrived, and those still being read and holding the end.
	#streams = 0;
	#active = 0;
	// Streams still being read that the end waits for at most the grace, until a settle gives up
	// on them, as the ledger does on a gap.
	#excused = new Set<object>();
	#changed = new Signal(0);
	#grace: () => Duration;
	// Groups whose stream finished since the last update was sent. A lite-07 stale drop under that
	// update names one only if it was sent before the publisher served the group, so the group won
	// over it, even once the cache evicted it.
	#finished: Run[] = [];
	// The span of the finished runs a fold under the pool's debt forgot since the last update. A
	// stale drop there for a group the ledger already accounted may be one its stream overtook, so
	// the next update owes none of it again.
	#folded?: Run;
	// What `#finished` bills, `READER_RUN_OVERHEAD` per run, so a publisher fragmenting the
	// streams it finishes between updates costs what the cache would hold.
	#bill?: Reader;

	constructor(options: TailOptions = {}) {
		this.#grace = options.grace ?? (() => GRACE);
		this.#bill = options.bill;
	}

	// Bill what `#finished` keeps, and under the pool's debt fold it. Mirrors the Rust `Tail::rebill`.
	#rebill(): void {
		const bill = this.#bill;
		if (!bill) return;
		bill.set((this.#finished.length + (this.#folded ? 1 : 0)) * READER_RUN_OVERHEAD);
		if (bill.debt > 0 && this.fold()) bill.set(2 * READER_RUN_OVERHEAD);
	}

	/**
	 * Fold all but the newest finished run into the span a fold forgot, returning whether any was:
	 * a stale drop under this update then stands as the publisher judged it, but the update owes
	 * none of what it names that {@link overtaken} reports. The newest run stays, so streams
	 * finishing in sequence grow nothing to charge again.
	 */
	fold(): boolean {
		if (this.#finished.length < 2) return false;
		const start = this.#finished[0].start;
		const end = this.#finished[this.#finished.length - 2].end;
		const folded = this.#folded;
		this.#folded = folded ? { start: min(folded.start, start), end: max(folded.end, end) } : { start, end };
		this.#finished = this.#finished.slice(-1);
		return true;
	}

	/** Release what the tail bills, once its subscription ended. */
	close(): void {
		this.#finished = [];
		this.#folded = undefined;
		// A stream still in flight finishes after this: with no bill, it can never bill again.
		this.#bill?.close();
		this.#bill = undefined;
	}

	/** Group streams whose header arrived, whether they finished or were reset. */
	get streams(): number {
		return this.#streams;
	}

	/** Record a group stream whose header arrived, held until it is read to its end. */
	open(sequence: bigint): Counted {
		this.#streams += 1;
		this.#active += 1;
		this.#account(sequence, sequence + 1n);

		let state: "held" | "excused" | "done" = "held";
		const token = {};
		return {
			done: () => {
				if (state === "done") return;
				if (state === "held") this.#active -= 1;
				else this.#excused.delete(token);
				state = "done";
				this.#bump();
			},
			excuse: () => {
				if (state !== "held") return;
				state = "excused";
				this.#active -= 1;
				this.#excused.add(token);
				this.#bump();
			},
			restore: () => {
				if (state !== "excused") return;
				state = "held";
				this.#excused.delete(token);
				this.#active += 1;
				this.#bump();
			},
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

	/** Record that the group stream for `sequence` arrived whole. */
	finish(sequence: bigint): void {
		insertRun(this.#finished, sequence, sequence + 1n);
		this.#rebill();
	}

	/** The parts of `[start, end)` whose stream did not finish since the last update was sent. */
	unfinished(start: bigint, end: bigint): Run[] {
		return runGaps(this.#finished, start, end);
	}

	/**
	 * The parts of `[start, end)` inside the span a fold forgot that the ledger accounts for, asked
	 * before a stale drop of them is accounted: the header of each arrived, or the grace gave up on
	 * it, before the drop, so the drop may be one a finished stream overtook.
	 */
	overtaken(start: bigint, end: bigint): Run[] {
		const folded = this.#folded;
		if (!folded) return [];
		const from = max(start, folded.start);
		const to = min(end, folded.end);
		if (from >= to) return [];
		return runGaps(this.#ledger.gaps(from, to), from, to);
	}

	/**
	 * Forget which streams finished, for an update being sent: a stale drop under an earlier one is
	 * no verdict, so none needs them.
	 */
	updated(): void {
		this.#finished = [];
		this.#folded = undefined;
		this.#rebill();
	}

	/** Re-check what {@link settle} waits on, after something outside the ledger changed. */
	touch(): void {
		this.#bump();
	}

	/** Record sequences `[start, end)` as accounted for without a stream: dropped, or a datagram. */
	account(start: bigint, end: bigint): void {
		this.#account(start, end);
	}

	/** Owe `[start, end)` again: a stale drop that accounted for it was reversed by a widening. */
	unaccount(start: bigint, end: bigint): void {
		this.#ledger.unaccount(start, end, now());
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: bigint, end: bigint): void {
		this.#ledger.demand(start, end, now());
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: bigint, end: bigint): boolean {
		return this.#ledger.covers(start, end);
	}

	/**
	 * Wait until every stream is read to its end and `complete()` holds, or the grace passes
	 * with nothing left being read but excused streams, or `closed` settles.
	 *
	 * A stream still being read is waited for: a group ends on its own stream's FIN or reset,
	 * never because its track ended. The grace gives up on streams that never arrived, and on
	 * excused ones: a browser can lose a stream's reset, leaving it open forever, and the
	 * publisher's drop already accounted for its group.
	 */
	async settle(complete: () => boolean, closed: GetPromise<unknown>): Promise<void> {
		// A gap that aged past the grace since the last insert is no longer waited for either.
		this.#ledger.expire(now(), this.#grace());

		let expired = false;
		const timer = new AbortController();
		void sleep(this.#grace(), { signal: timer.signal }).then(
			() => {
				expired = true;
				this.#bump();
			},
			() => {},
		);
		try {
			while (closed.peek() === undefined) {
				if (this.#active === 0 && expired) {
					// Given up on, so a later settle, such as the one the publisher's FIN starts, does
					// not wait another grace for them.
					this.#excused.clear();
					return;
				}
				if (this.#active === 0 && this.#excused.size === 0 && complete()) return;
				await Signal.race(this.#changed, closed);
			}
		} finally {
			timer.abort();
		}
	}

	#account(start: bigint, end: bigint): void {
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
