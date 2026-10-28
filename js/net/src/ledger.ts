import { Instant, Milli, Nano } from "@moq/clock";
import * as btree from "sorted-btree";

// sorted-btree ships only CommonJS: Node binds its default import to `module.exports`,
// while Bun and bundlers bind it to the class.
const BTree = (btree.default as unknown as { default?: typeof btree.default }).default ?? btree.default;

/**
 * How long a subscriber waits for a group stream it cannot account for once the publisher
 * has ended the subscription.
 *
 * A group reset before its header arrived leaves no trace, and QUIC does not order streams,
 * so a stream opened before the end can still be in flight after it. This bounds the wait on
 * IETF, and on moq-lite when the subscription has no max delay to bound it with.
 */
export const GRACE = Milli(1000);

/** The grace for a subscription with `maxDelay`: how long it was willing to wait for a late group anyway, or {@link GRACE} without one. */
export function grace(maxDelay: Milli): Milli {
	return maxDelay > 0 ? maxDelay : GRACE;
}

// A run of accounted sequences `[start, end)`, and when the gap below it opened, which is
// when its missing groups became late.
type Run = { start: number; end: number; since: Instant };

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
	#runs = new BTree<number, Run>(undefined, (a, b) => a - b);
	// Demand can restart a gap below an older one. Index ages separately so expiring
	// one gap never scans all the younger ones. Both indexes own exactly the same runs.
	#ages = new BTree<Run, undefined>(undefined, (a, b) => a.since - b.since || a.start - b.start);
	#last?: Run;
	// Exclude the leading gap, which has no lower run to fold into.
	#next?: Run;

	#remove(run: Run): void {
		this.#runs.delete(run.start);
		this.#ages.delete(run);
	}

	#nextExpiry(): void {
		const first = this.#runs.minKey();
		const oldest = this.#ages.minKey();
		this.#next = oldest?.start === first ? this.#ages.nextHigherKey(oldest) : oldest;
	}

	/** Record sequences `[start, end)` as accounted for. Gaps fold only on {@link expire}. */
	account(start: number, end: number, at: Instant): void {
		if (start >= end) return;

		// An in-order feed extends the last run in place, the usual case for every arrival.
		const last = this.#last;
		if (last && last.start <= start && start <= last.end) {
			last.end = Math.max(last.end, end);
			return;
		}

		const lower = this.#runs.getPairOrNextLower(start)?.[1];
		const below = lower && lower.end >= start ? lower : undefined;
		if (below && end <= below.end) return;

		// Splitting a gap inherits its age; only an arrival past all runs opens a new one.
		const run = below ?? { start, end, since: this.#runs.getPairOrNextHigher(start)?.[1].since ?? at };
		run.end = Math.max(run.end, end);
		let next = this.#runs.getPairOrNextHigher(start);
		if (next?.[1] === below) next = this.#runs.nextHigherPair(start);
		while (next && next[0] <= run.end) {
			run.end = Math.max(run.end, next[1].end);
			this.#remove(next[1]);
			next = this.#runs.nextHigherPair(next[0]);
		}
		if (!below) {
			this.#runs.set(run.start, run);
			this.#ages.set(run, undefined);
		}
		if (!next) this.#last = run;
		this.#nextExpiry();
	}

	/** Fold every gap at least `grace` old into the runs around it. */
	expire(at: Instant, grace: Milli): void {
		const limit = Nano.fromMilli(grace);
		while (this.#next && Instant.durationSince(at, this.#next.since) >= limit) {
			const run = this.#next;
			const below = this.#runs.nextLowerPair(run.start)?.[1];
			if (!below) throw new Error("an expiring gap must have a lower run");
			below.end = run.end;
			this.#remove(run);
			if (this.#last === run) this.#last = below;
			this.#nextExpiry();
		}
	}

	/** Whether a gap separates two runs, so {@link expire} has anything to fold. */
	hasGaps(): boolean {
		return this.#runs.size > 1;
	}

	/** The lowest sequence accounted for. */
	first(): number | undefined {
		return this.#runs.minKey();
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: number, end: number): boolean {
		if (start >= end) return true;
		// Runs merge on insert, so one run covers the span or none does.
		const run = this.#runs.getPairOrNextLower(start)?.[1];
		return run !== undefined && end <= run.end;
	}

	/**
	 * The gaps within `[start, end)`, each with when a later arrival opened it: none for the one
	 * past the last run, which nothing has opened yet.
	 */
	gaps(start: number, end: number): { start: number; end: number; since?: Instant }[] {
		const gaps: { start: number; end: number; since?: Instant }[] = [];
		let below = start;
		const lower = this.#runs.getPairOrNextLower(start)?.[1];
		for (const [, run] of this.#runs.entries(lower?.start ?? start)) {
			if (below >= end) return gaps;
			if (run.end <= below) continue;
			if (below < run.start) gaps.push({ start: below, end: Math.min(run.start, end), since: run.since });
			below = run.end;
		}
		if (below < end) gaps.push({ start: below, end });
		return gaps;
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: number, end: number, at: Instant): void {
		if (start >= end) return;
		const lower = this.#runs.getPairOrNextLower(start)?.[1];
		let below = lower?.end ?? 0;
		for (const [, run] of this.#runs.entries(start)) {
			if (below >= end) break;
			if (start < run.start) {
				this.#ages.delete(run);
				run.since = at;
				this.#ages.set(run, undefined);
			}
			below = run.end;
		}
		this.#nextExpiry();
	}
}
