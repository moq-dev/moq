import { type Duration, now, sleep } from "@moq/clock";
import { type GetPromise, type Getter, Signal } from "@moq/signals";
import { insertRun, type Run, runGaps, runsFrom } from "./internal.ts";
import { GRACE, Ledger, MAX_RUNS } from "./ledger.ts";

/** How a {@link Tail} measures its grace. */
export interface TailOptions {
	/**
	 * How long to wait for a group that cannot be accounted for, or `undefined` to wait until the
	 * wire accounts for it (lite-07, which names every start). Read each time it is needed.
	 */
	grace?: () => Duration | undefined;
}

/** A group stream a {@link Tail} counts, from its header until it is read to its end. */
export interface Counted {
	/** Stop holding the end open for it: read to its end, or a drop accounted for its group. Idempotent. */
	done(): void;
	/** Stop holding the end open while it waits, unread, on a copy another stream delivers. Idempotent. */
	park(): void;
	/** Hold the end open again once it reads the group itself. Idempotent. */
	resume(): void;
}

/**
 * The group streams a subscription has received, so its end can wait for the ones still owed.
 *
 * A publisher ends a subscription before every group stream it opened has necessarily
 * arrived. This records which keys are accounted for (a stream's header arrived, or
 * the publisher dropped them) and how many streams are still being read, and {@link settle}
 * waits on them.
 *
 * A lost datagram is not owed: it leaves a hole that waits out the grace like a stream reset
 * before its header.
 *
 * @internal
 */
export class Tail {
	// The keys accounted for, whose gaps fold once the grace stops waiting for them.
	#ledger = new Ledger();
	// Group streams whose header arrived, and those still being read and holding the end.
	#streams = 0;
	#active = 0;
	// The lowest group a stream's header named, which proves the group exists.
	#first: bigint | undefined;
	#changed = new Signal(0);
	#grace: () => Duration | undefined;
	// Keys a drop named, so a stream whose header the drop overtook can be told from one
	// whose gap the grace gave up on.
	#dropped: Run[] = [];

	constructor(options: TailOptions = {}) {
		this.#grace = options.grace ?? (() => GRACE);
	}

	/** Release what the tail keeps beside its ledger, once its subscription ended. */
	close(): void {
		this.#dropped = [];
	}

	/** Group streams whose header arrived, whether they finished or were reset. */
	get streams(): number {
		return this.#streams;
	}

	/** The lowest group whose stream's header arrived. */
	get first(): bigint | undefined {
		return this.#first;
	}

	/** Record a group stream whose header arrived, held until it is read to its end. */
	open(key: bigint): Counted {
		this.#streams += 1;
		if (this.#first === undefined || key < this.#first) this.#first = key;
		this.#active += 1;
		this.#account(key, key + 1n);

		// Parked holds nothing.
		let state: "held" | "parked" | "done" = "held";
		return {
			done: () => {
				if (state === "done") return;
				if (state === "held") this.#active -= 1;
				state = "done";
				this.#bump();
			},
			park: () => {
				if (state !== "held") return;
				state = "parked";
				this.#active -= 1;
				this.#bump();
			},
			resume: () => {
				if (state !== "parked") return;
				state = "held";
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

	/** Re-check what {@link settle} waits on, after something outside the ledger changed. */
	touch(): void {
		this.#bump();
	}

	/** Record keys `[start, end)` as accounted for without a stream: dropped, or a datagram. */
	account(start: bigint, end: bigint): void {
		this.#account(start, end);
	}

	/** Record `[start, end)` as given up by a drop, accounting for it. */
	drop(start: bigint, end: bigint): void {
		insertRun(this.#dropped, start, end);
		while (this.#dropped.length > MAX_RUNS) this.#dropped.shift();
		this.#account(start, end);
	}

	/** Whether a drop named `key`. */
	dropped(key: bigint): boolean {
		const run = this.#dropped[runsFrom(this.#dropped, key)];
		return run !== undefined && run.start <= key;
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: bigint, end: bigint): void {
		this.#ledger.demand(start, end, now());
	}

	/** The parts of `[start, end)` accounted for: a stream arrived, a drop named them, or the grace gave up. */
	accounted(start: bigint, end: bigint): Run[] {
		return runGaps(this.#ledger.gaps(start, end), start, end);
	}

	/** Whether a group stream is still being read. */
	get reading(): boolean {
		return this.#active > 0;
	}

	/** Bumped on every change {@link settle} watches. */
	get changed(): Getter<number> {
		return this.#changed;
	}

	/** Whether every key in `[start, end)` is accounted for. */
	covers(start: bigint, end: bigint): boolean {
		return this.#ledger.covers(start, end);
	}

	/**
	 * Wait until every stream is read to its end and `complete()` holds, or the grace passes
	 * with nothing left being read, or `closed` settles.
	 *
	 * A stream still being read is waited for, until its own FIN or reset, or until a drop naming
	 * its group lets go of it (see {@link Counted.done}). The grace gives up on streams that never
	 * arrived, and `limit` caps it. With no grace nothing is timed: every start waits for the wire
	 * to account for it.
	 */
	async settle(complete: () => boolean, closed: GetPromise<unknown>, limit?: Duration): Promise<void> {
		// A gap that aged past the grace since the last insert is no longer waited for either.
		const grace = this.#grace();
		this.#ledger.expire(now(), grace);

		let expired = false;
		const timer = new AbortController();
		if (grace !== undefined) {
			void sleep(limit === undefined ? grace : grace.min(limit), { signal: timer.signal }).then(
				() => {
					expired = true;
					this.#bump();
				},
				() => {},
			);
		}
		try {
			while (closed.peek() === undefined) {
				if (this.#active === 0 && (expired || complete())) return;
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
