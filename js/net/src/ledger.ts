import { Instant, Milli, Nano } from "@moq/clock";

/**
 * How long a subscriber waits for a group stream it cannot account for once the publisher
 * has ended the subscription.
 *
 * A group reset before its header arrived leaves no trace, and QUIC does not order streams,
 * so a stream opened before the end can still be in flight after it. This bounds the wait on
 * IETF, and on moq-lite when the subscription has no max delay to bound it with.
 */
export const GRACE = Milli(1000);

// private: plumbing
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
	#runs: Run[] = [];

	/** Record sequences `[start, end)` as accounted for. Gaps fold only on {@link expire}. */
	account(start: number, end: number, at: Instant): void {
		if (start >= end) return;

		// An in-order feed extends the last run in place, the usual case for every arrival.
		const last = this.#runs.at(-1);
		if (last && last.start <= start && start <= last.end) {
			last.end = Math.max(last.end, end);
			return;
		}

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
		if (!placed) runs.push(merged ?? { start, end, since: at });
		this.#runs = runs;
	}

	/** Fold every gap at least `grace` old into the runs around it. */
	expire(at: Instant, grace: Milli): void {
		const limit = Nano.fromMilli(grace);
		const runs: Run[] = [];
		for (const run of this.#runs) {
			const below = runs.at(-1);
			if (below && Instant.durationSince(at, run.since) >= limit) {
				runs[runs.length - 1] = { ...below, end: run.end };
			} else {
				runs.push(run);
			}
		}
		this.#runs = runs;
	}

	/** Whether a gap separates two runs, so {@link expire} has anything to fold. */
	hasGaps(): boolean {
		return this.#runs.length > 1;
	}

	/** The lowest sequence accounted for. */
	first(): number | undefined {
		return this.#runs[0]?.start;
	}

	/** Whether every sequence in `[start, end)` is accounted for. */
	covers(start: number, end: number): boolean {
		if (start >= end) return true;
		// Runs merge on insert, so one run covers the span or none does.
		return this.#runs.some((run) => run.start <= start && end <= run.end);
	}

	/** Restart the age of every gap reaching into `[start, end)`, which the demand newly asks for. */
	demand(start: number, end: number, at: Instant): void {
		if (start >= end) return;
		let below = 0;
		this.#runs = this.#runs.map((run) => {
			const reached = below < end && start < run.start;
			below = run.end;
			return reached ? { ...run, since: at } : run;
		});
	}
}
