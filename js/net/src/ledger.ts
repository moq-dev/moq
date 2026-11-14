import { Duration, type Instant } from "@moq/clock";
import { max, min } from "./util/sequence.ts";
import { Sorted } from "./util/sorted.ts";

const compare = (a: bigint, b: bigint): number => (a < b ? -1 : a > b ? 1 : 0);

/**
 * How long a subscriber waits for a group stream it cannot account for once the publisher
 * has ended the subscription.
 *
 * A group reset before its header arrived leaves no trace, and QUIC does not order streams,
 * so a stream opened before the end can still be in flight after it. This bounds the wait on
 * IETF, and on moq-lite when the subscription has no max delay to bound it with.
 */
export const GRACE = Duration.millis(1000);

/** The grace for a subscription with `delay.max`: how long it was willing to wait for a late group anyway, or {@link GRACE} without one. */
export function grace(maxDelay: Duration): Duration {
	return maxDelay.isZero() ? GRACE : maxDelay;
}

// Bun 1.4's heap (bun:jsc `heapStats`) grows about 191 bytes per disjoint run of a ledger, its
// age entry and timestamp included.
/** Bookkeeping a track bills per run of its arrivals, so a peer fragmenting them costs what the cache would hold. */
export const RUN_OVERHEAD = 192;

// A run of accounted sequences `[start, end)`, and when the gap below it opened, which is
// when its missing groups became late.
type Run = { start: bigint; end: bigint; since: Instant };

/**
 * The sequences a feed has accounted for, and how long each gap between them is waited for.
 *
 * A subscription's `Tail` and a track's record of live arrivals both keep one, so they
 * give up on a missing group by the same rule. Mirrors the Rust `Ledger`.
 *
 * @internal
 */
export class Ledger {
	// Disjoint, sorted, non-adjacent runs of accounted sequences. A gap older than the grace
	// can no longer be waited for, so it folds into the runs around it, which bounds this by
	// the gaps opened within the grace rather than every gap ever seen. A fold is final: a
	// later, longer grace or a lowered floor cannot reopen it.
	#runs = new Sorted<bigint, Run>((run) => run.start, compare);
	// Demand can restart a gap below an older one. Index ages separately so expiring
	// one gap never scans all the younger ones. Both indexes own exactly the same runs.
	#ages = new Sorted<Run, Run>(
		(run) => run,
		(a, b) => a.since.cmp(b.since) || compare(a.start, b.start),
	);
	#last?: Run;
	// Exclude the leading gap, which has no lower run to fold into.
	#next?: Run;

	#insert(run: Run): void {
		this.#runs.insert(run);
		this.#ages.insert(run);
	}

	#remove(run: Run): void {
		this.#runs.delete(run.start);
		this.#ages.delete(run);
	}

	#nextExpiry(): void {
		const oldest = this.#ages.first();
		this.#next = oldest && oldest === this.#runs.first() ? this.#ages.higher(oldest) : oldest;
	}

	/** Record sequences `[start, end)` as accounted for. Gaps fold only on {@link expire}. */
	account(start: bigint, end: bigint, at: Instant): void {
		if (start >= end) return;

		// An in-order feed extends the last run in place, the usual case for every arrival.
		const last = this.#last;
		if (last && last.start <= start && start <= last.end) {
			last.end = max(last.end, end);
			return;
		}

		const lower = this.#runs.floor(start);
		const below = lower && lower.end >= start ? lower : undefined;
		if (below && end <= below.end) return;

		// Splitting a gap inherits its age; only an arrival past all runs opens a new one.
		const run = below ?? { start, end, since: this.#runs.ceil(start)?.since ?? at };
		run.end = max(run.end, end);
		let next = this.#runs.ceil(start);
		if (next === below) next = this.#runs.higher(start);
		while (next && next.start <= run.end) {
			run.end = max(run.end, next.end);
			this.#remove(next);
			next = this.#runs.higher(next.start);
		}
		if (!below) this.#insert(run);
		if (!next) this.#last = run;
		this.#nextExpiry();
	}

	/** Fold every gap at least `grace` old into the runs around it. */
	expire(at: Instant, grace: Duration): void {
		while (this.#next && at.since(this.#next.since).ge(grace)) {
			const run = this.#next;
			const below = this.#runs.lower(run.start);
			if (!below) throw new Error("an expiring gap must have a lower run");
			below.end = run.end;
			this.#remove(run);
			if (this.#last === run) this.#last = below;
			this.#nextExpiry();
		}
	}

	/**
	 * Fold the oldest gap into the runs around it however young, giving up on it early, and
	 * return the sequences it held; undefined when no gap is left to fold.
	 */
	foldOldest(): { start: bigint; end: bigint } | undefined {
		const run = this.#next;
		if (!run) return undefined;
		const below = this.#runs.lower(run.start);
		if (!below) throw new Error("a folding gap must have a lower run");
		const gap = { start: below.end, end: run.start };
		below.end = run.end;
		this.#remove(run);
		if (this.#last === run) this.#last = below;
		this.#nextExpiry();
		return gap;
	}

	/** Whether a gap separates two runs, so {@link expire} has anything to fold. */
	hasGaps(): boolean {
		return this.#runs.size > 1;
	}

	/** How many runs it holds: one more than its gaps. */
	get size(): number {
		return this.#runs.size;
	}

	/** The lowest sequence accounted for. */
	first(): bigint | undefined {
		return this.#runs.first()?.start;
	}

	/** The exclusive end of the run accounting for `sequence`, undefined when none does. */
	runEnd(sequence: bigint): bigint | undefined {
		const run = this.#runs.floor(sequence);
		return run !== undefined && sequence < run.end ? run.end : undefined;
	}

	/** The exclusive end of the highest run starting at or below `sequence`. */
	endBelow(sequence: bigint): bigint | undefined {
		return this.#runs.floor(sequence)?.end;
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: bigint, end: bigint): boolean {
		if (start >= end) return true;
		// Runs merge on insert, so one run covers the span or none does.
		const run = this.#runs.floor(start);
		return run !== undefined && end <= run.end;
	}

	/**
	 * The gaps within `[start, end)`, each with when a later arrival opened it: none for the one
	 * past the last run, which nothing has opened yet.
	 */
	gaps(start: bigint, end: bigint): { start: bigint; end: bigint; since?: Instant }[] {
		const gaps: { start: bigint; end: bigint; since?: Instant }[] = [];
		let below = start;
		const lower = this.#runs.floor(start);
		for (const run of this.#runs.from(lower?.start ?? start)) {
			if (below >= end) return gaps;
			if (run.end <= below) continue;
			if (below < run.start) gaps.push({ start: below, end: min(run.start, end), since: run.since });
			below = run.end;
		}
		if (below < end) gaps.push({ start: below, end });
		return gaps;
	}

	/**
	 * Take `[start, end)` back out of what is accounted for, owed again from `at`: a verdict that
	 * accounted for it was reversed. The gap it leaves opens at `at`, so it is waited for a fresh
	 * grace, even where it joins an older gap. Mirrors the Rust `Ledger::unaccount`.
	 */
	unaccount(start: bigint, end: bigint, at: Instant): void {
		if (start >= end) return;
		const lower = this.#runs.floor(start);
		const overlapping: Run[] = [];
		for (const run of this.#runs.from(lower?.start ?? start)) {
			if (run.start >= end) break;
			if (run.end > start) overlapping.push(run);
		}
		if (overlapping.length === 0) return;
		for (const run of overlapping) {
			this.#remove(run);
			if (this.#last === run) this.#last = undefined;
			if (run.start < start) {
				this.#insert({ start: run.start, end: start, since: run.since });
			}
			if (end < run.end) {
				this.#insert({ start: end, end: run.end, since: at });
			}
		}
		this.#last = this.#runs.last();
		this.demand(start, end, at);
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: bigint, end: bigint, at: Instant): void {
		if (start >= end) return;
		const lower = this.#runs.floor(start);
		let below = lower?.end ?? 0n;
		for (const run of this.#runs.from(start)) {
			if (below >= end) break;
			if (start < run.start) {
				this.#ages.delete(run);
				run.since = at;
				this.#ages.insert(run);
			}
			below = run.end;
		}
		this.#nextExpiry();
	}
}
