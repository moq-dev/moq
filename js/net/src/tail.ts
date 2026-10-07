import { type GetPromise, Signal } from "@moq/signals";
import { Milli } from "./time.ts";

/**
 * How long a subscriber waits for a group stream it cannot account for once the publisher
 * has ended the subscription.
 *
 * A group reset before its header arrived leaves no trace, and QUIC does not order streams,
 * so a stream opened before the end can still be in flight after it. This bounds the wait on
 * IETF, and on moq-lite when the subscription has no max delay to bound it with.
 */
export const TAIL_GRACE_MS = Milli(1000);

// setTimeout truncates a longer delay to a signed 32-bit int and fires at once.
const MAX_TIMEOUT_MS = 2 ** 31 - 1;

// A run of accounted sequences `[start, end)`, and when the gap below it opened (in
// `performance.now()` milliseconds), which is when its missing groups became late.
type Run = { start: number; end: number; since: number };

/** How a {@link Tail} measures its grace. */
export interface TailOptions {
	/** How long to wait for a group that cannot be accounted for. Read each time it is needed. */
	grace?: () => Milli;
	/** The clock, in `performance.now()` milliseconds. */
	now?: () => number;
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
	// Disjoint, sorted, non-adjacent runs of accounted sequences. A gap older than the grace
	// can no longer be waited for, so it folds into the runs around it, which bounds this by
	// the gaps opened within the grace rather than every gap in the subscription. A fold is
	// final: a later, longer grace or a lowered floor cannot reopen it.
	#runs: Run[] = [];
	// Group streams whose header arrived, and those still being read.
	#streams = 0;
	#active = 0;
	#changed = new Signal(0);
	#grace: () => Milli;
	#now: () => number;

	constructor(options: TailOptions = {}) {
		this.#grace = options.grace ?? (() => TAIL_GRACE_MS);
		this.#now = options.now ?? (() => performance.now());
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

	/** Record sequences `[start, end)` as accounted for without a stream: dropped, or a datagram. */
	account(start: number, end: number): void {
		this.#account(start, end);
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: number, end: number): void {
		if (start >= end) return;
		const now = this.#now();
		let below = 0;
		this.#runs = this.#runs.map((run) => {
			const reached = below < end && start < run.start;
			below = run.end;
			return reached ? { ...run, since: now } : run;
		});
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: number, end: number): boolean {
		if (start >= end) return true;
		// Runs are merged on insert, so one run covers the span or none does.
		return this.#runs.some((run) => run.start <= start && end <= run.end);
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
		this.#expire(this.#now());

		let expired = false;
		const timer = setTimeout(
			() => {
				expired = true;
				this.#bump();
			},
			Math.min(this.#grace(), MAX_TIMEOUT_MS),
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

		const now = this.#now();
		const runs: Run[] = [];
		let merged: Run | undefined;
		let placed = false;
		for (const run of this.#runs) {
			if (run.end < start) {
				runs.push(run);
			} else if (end < run.start) {
				// Splitting a gap leaves both halves as late as it was.
				if (!placed) runs.push(merged ?? { start, end, since: run.since });
				placed = true;
				runs.push(run);
			} else {
				merged = {
					start: Math.min(merged?.start ?? start, run.start),
					end: Math.max(merged?.end ?? end, run.end),
					since: merged?.since ?? run.since,
				};
			}
		}
		// A run past every other one opens a new gap.
		if (!placed) runs.push(merged ?? { start, end, since: now });
		this.#runs = runs;
		this.#expire(now);
		this.#bump();
	}

	// Fold every gap older than the grace into the runs around it.
	#expire(now: number): void {
		const grace = this.#grace();
		const runs: Run[] = [];
		for (const run of this.#runs) {
			const below = runs.at(-1);
			if (below && now - run.since >= grace) {
				runs[runs.length - 1] = { ...below, end: run.end };
			} else {
				runs.push(run);
			}
		}
		this.#runs = runs;
	}

	#bump(): void {
		this.#changed.update((revision) => revision + 1);
	}
}
