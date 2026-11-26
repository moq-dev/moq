import { type Duration, now, sleep } from "@moq/clock";
import { type GetPromise, type Getter, Signal } from "@moq/signals";
import { insertRun, type Run, runGaps, runsFrom } from "./internal.ts";
import { GRACE, Ledger } from "./ledger.ts";

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
	/** Mark its end read, or its span accounted for another way. Idempotent. */
	finish(): void;
	/**
	 * Let go of it, read or not. One whose end went unread leaves the rest of its span for the
	 * publisher to name again; see {@link Tail.takeLost}. Idempotent.
	 */
	done(): void;
	/**
	 * The publisher accounted for its group another way, such as a drop, so {@link Tail.settle}
	 * waits for it at most the grace. Idempotent.
	 */
	excuse(): void;
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
	// Streams still being read that the end waits for at most the grace, until a settle gives up
	// on them, as the ledger does on a gap.
	#excused = new Set<object>();
	#changed = new Signal(0);
	#grace: () => Duration | undefined;
	// Keys a drop named, so a stream whose header the drop overtook can be told from one
	// whose gap the grace gave up on.
	#dropped: Run[] = [];
	// With no grace (lite-07): groups whose stream was let go before its end, which leaves the
	// rest of the span its GROUP_END told unnamed, for a SUBSCRIBE_UPDATE to ask the publisher
	// to name again, once each.
	#lost = new Set<bigint>();
	#lostChanged = new Signal(0);

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

		// Parked holds nothing, excused or not: an excuse made while parked applies once it resumes.
		let state: "held" | "excused" | "parked" | "parked-excused" | "done" = "held";
		let unread = true;
		const token = {};
		return {
			finish: () => {
				unread = false;
			},
			done: () => {
				if (state === "held") this.#active -= 1;
				else if (state === "excused") this.#excused.delete(token);
				else if (state === "done") return;
				state = "done";
				if (unread) this.#lose(key);
				this.#bump();
			},
			excuse: () => {
				if (state === "parked") state = "parked-excused";
				if (state !== "held") return;
				state = "excused";
				this.#active -= 1;
				this.#excused.add(token);
				this.#bump();
			},
			park: () => {
				if (state === "held") this.#active -= 1;
				else if (state === "excused") this.#excused.delete(token);
				else return;
				state = state === "held" ? "parked" : "parked-excused";
				this.#bump();
			},
			resume: () => {
				if (state === "parked") this.#active += 1;
				else if (state === "parked-excused") this.#excused.add(token);
				else return;
				state = state === "parked" ? "held" : "excused";
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
		this.#account(start, end);
	}

	/** Whether a let-go stream left a group for the publisher to name the rest of again. */
	get hasLost(): boolean {
		return this.#lost.size > 0;
	}

	/** Bumped whenever a stream is lost; see {@link takeLost}. */
	get lostChanged(): Getter<number> {
		return this.#lostChanged;
	}

	/** Take the groups a SUBSCRIBE_UPDATE asks the publisher to name the rest of again. */
	takeLost(): bigint[] {
		const lost = [...this.#lost];
		this.#lost.clear();
		return lost;
	}

	// The stream of `key` was let go before its end. With no grace, the publisher is asked to name
	// the rest of its span again: only it knows where the span ends, as the next start being
	// accounted for proves nothing when a skip or drop named it. Mirrors the Rust `Tail::lose`.
	#lose(key: bigint): void {
		if (this.#grace() !== undefined || this.#lost.has(key)) return;
		this.#lost.add(key);
		this.#lostChanged.update((n) => n + 1);
	}

	/** Whether a drop named `key`. */
	dropped(key: bigint): boolean {
		const run = this.#dropped[runsFrom(this.#dropped, key)];
		return run !== undefined && run.start <= key;
	}

	/**
	 * Account every gap of a tail with no grace that no run of `owed` (sorted, disjoint) reaches, as
	 * when a range retires: its publisher names nothing outside its live ranges, so with no grace to
	 * fold them, those gaps would otherwise outlive the ranges that left them. Mirrors the Rust
	 * `Tail::retire`.
	 */
	retire(owed: readonly Run[]): void {
		if (this.#grace() !== undefined) return;
		this.#ledger.retire(owed, now());
		this.#bump();
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: bigint, end: bigint): void {
		this.#ledger.demand(start, end, now());
	}

	/** The parts of `[start, end)` accounted for: a stream arrived, a drop named them, or the grace gave up. */
	accounted(start: bigint, end: bigint): Run[] {
		return runGaps(this.#ledger.gaps(start, end), start, end);
	}

	/** Whether a group stream is still being read, an excused one included until a settle gives up on it. */
	get reading(): boolean {
		return this.#active > 0 || this.#excused.size > 0;
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
	 * with nothing left being read but excused streams, or `closed` settles.
	 *
	 * A stream still being read is waited for: a group ends on its own stream's FIN or reset,
	 * never because its track ended. The grace gives up on streams that never arrived, and on
	 * excused ones: a browser can lose a stream's reset, leaving it open forever, and the
	 * publisher's drop already accounted for its group. `limit` caps the grace, and gives up on
	 * every missing start even with no grace, as the Rust `Settle::at_most` does. With neither,
	 * only excused streams are given up, after {@link GRACE}: every other start waits for the
	 * wire to account for it.
	 */
	async settle(complete: () => boolean, closed: GetPromise<unknown>, limit?: Duration): Promise<void> {
		// A gap that aged past the grace since the last insert is no longer waited for either.
		const grace = this.#grace();
		this.#ledger.expire(now(), grace);

		let expired = false;
		const timer = new AbortController();
		const wait = grace === undefined ? (limit ?? GRACE) : limit === undefined ? grace : grace.min(limit);
		void sleep(wait, { signal: timer.signal }).then(
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
					if (grace !== undefined || limit !== undefined) return;
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
