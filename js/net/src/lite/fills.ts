// Fills a lite-07 copy asks of its upstream through a subscription rather than FETCH. Mirrors the
// Rust `lite/subscriber/fills.rs`.

import { type GetPromise, type Getter, race, Signal } from "@moq/signals";
import { NotFound, StreamCode, toStreamCode } from "../error.ts";
import { fillChanged, wantedRuns } from "../fill.ts";
import { hooks, insertRun, type Run, removeRun, runGaps } from "../internal.ts";
import type { Tail } from "../tail.ts";
import type * as track from "../track.ts";
import { max, min } from "../util/sequence.ts";

/** The lowest priority, which any fill's outranks. */
const NO_PRIORITY = -128;

/**
 * Why the subscription that asked for groups will not deliver them.
 *
 * `lost` when the source no longer has them, which the track records so no reader asks again:
 * only a genuine miss its publisher named. Anything else answers only the fills owing them now,
 * since another request may still get them: a stale drop judges a budget, and a range starting
 * past them is the subscriber's own choice.
 */
export interface Refusal {
	error: Error;
	lost: boolean;
}

/** Groups a subscription refused, and why. */
export interface Refused extends Run {
	refusal: Refusal;
}

/** The publisher gave the groups up with `error`: lost only when it never had them. */
export function dropped(error: Error): Refusal {
	return { error, lost: toStreamCode(error) === StreamCode.NotFound };
}

/** Not delivered on this subscription, which says nothing of the source. */
export function passing(error: Error): Refusal {
	return { error, lost: false };
}

/** `runs` with `[start, end)` refused by `refusal` instead, so a repeated refusal costs nothing. */
export function refuseRun(runs: readonly Refused[], start: bigint, end: bigint, refusal: Refusal): Refused[] {
	if (start >= end) return [...runs];
	return [...removeRun(runs, start, end), { start, end, refusal }].sort(byStart);
}

/**
 * A subscription asking again, with no max delay, for groups another one dropped as stale or
 * already delivered, which a fill still owes: what the subscriber opens for a {@link RangeFills}.
 */
export interface RecoveryHandle {
	/** Resolves once it is over, with the groups it refused. */
	done: Promise<Refused[]>;
	/**
	 * The groups it refused since the last call, each handed over once, which answer the fills
	 * owing them before it is over. Its {@link done} resolves with only those never taken.
	 */
	takeRefused(): Refused[];
	/** Bumped each time it refuses more. */
	readonly changed: Getter<number>;
	/** Whether a stream of it is still being read, which cancelling would cut. */
	reading(): boolean;
	/** Stop asking: what is left is refused. */
	cancel(): void;
}

class Recovery {
	readonly groups: Run;
	readonly handle: RecoveryHandle;
	refused?: Refused[];
	// The groups it refused and handed over, which it no longer covers, so a later request for
	// them asks again rather than waiting on its other streams.
	readonly released: Run[] = [];

	// `ended` is called once it is over, so a wait need not hold a reaction on `done`.
	constructor(groups: Run, handle: RecoveryHandle, ended: () => void) {
		this.groups = groups;
		this.handle = handle;
		void handle.done.then((refused) => {
			this.refused = refused;
			ended();
		});
	}
}

/** What a lite-07 copy's held fills ask of its upstream subscription, beside the demand. */
export interface FillAsk {
	/** The groups to ask for, empty when no fill is held. */
	groups: Run[];
	/** The most urgent priority a held fill asks with, which the subscription takes if higher. */
	priority: number;
}

/**
 * Fills a lite-07 upstream serves through the copy's subscription rather than FETCH: each held
 * fill's next owed run joins the subscription's ranges until its groups arrive or the publisher
 * names them given up, and the fill answers itself from the cache.
 *
 * A fill is judged against the subscription that asked for it alone. The track's arrivals still
 * account for a group an earlier subscription cut, which this one may still deliver, so only what
 * this subscription says it will not deliver (a SUBSCRIBE_DROP, or a range start past the range)
 * refuses a fill. A range start speaks only for its range: once the range retires, a later one
 * asking for the same groups may start lower. A group it drops as stale is officially dropped,
 * and one it already accounted for (delivered before and since evicted) it never sends again, so
 * a recovery asks for either on its own subscription with no max delay, once. Nothing newer
 * judges it stale there, so once every hop forgot a run, a narrow reader takes its stale groups.
 *
 * A group stays asked until its stream finishes, since narrowing the subscription under a stream
 * still arriving would reset it.
 */
export class RangeFills {
	readonly #copy: track.Producer;
	readonly #recover: (groups: Run, priority: number) => RecoveryHandle;
	#held: track.Fill[] = [];
	/** What the subscription asks for the fills now. */
	ask: FillAsk = { groups: [], priority: NO_PRIORITY };
	// Bumped on every input `poll` reads that no other signal reports.
	readonly #bumped = new Signal(0);
	// Asked groups no fill owes any more whose stream is still arriving, and how each ends.
	#reading = new Map<bigint, GetPromise<Error | null>>();
	// Asked groups no fill owes any more, already searched for streams still arriving.
	#draining: Run[] = [];
	// Groups the subscription said it will not deliver, and why.
	#refused: Refused[] = [];
	// Groups below where a fill's range starts, with the asked run that range carries, which stand
	// only while that range is live.
	#skipped: (Run & { asked: Run })[] = [];
	// Groups dropped as stale, which a recovery has yet to ask for.
	#toRecover: Run[] = [];
	// Groups a recovery asks for now, which the subscription leaves out meanwhile.
	#recovering: Run[] = [];
	// Groups recovered once already, which a second stale drop refuses.
	#recovered: Run[] = [];
	#recoveries = new Set<Recovery>();
	#closed = false;

	constructor(copy: track.Producer, recover: (groups: Run, priority: number) => RecoveryHandle) {
		this.#copy = copy;
		this.#recover = recover;
	}

	/** Whether no fill is held, asked for, or recovering. */
	isEmpty(): boolean {
		return this.#held.length === 0 && this.ask.groups.length === 0 && this.#recoveries.size === 0;
	}

	/** Whether a recovery is still out. */
	get recovering(): boolean {
		return this.#recoveries.size > 0;
	}

	push(fill: track.Fill): void {
		this.#held.push(fill);
		this.#bump();
	}

	/**
	 * The subscription will not deliver `[start, end)`: a fill owing one is refused with `error`,
	 * or recovered once when it was dropped as stale.
	 */
	refuse(start: bigint, end: bigint, error: Error): void {
		// Only what a fill asked for: a fill asks for nothing the subscription dropped before it.
		for (const run of clip(this.ask.groups, start, end)) {
			if (toStreamCode(error) === StreamCode.Old) {
				const done = clip(this.#recovered, run.start, run.end);
				for (const fresh of runGaps(done, run.start, run.end))
					insertRun(this.#toRecover, fresh.start, fresh.end);
				// Recovered once already: this drop stands for the fills owing it now.
				for (const again of done) this.#setRefused(again.start, again.end, dropped(error));
				continue;
			}
			this.#setRefused(run.start, run.end, dropped(error));
		}
		this.#bump();
	}

	/**
	 * The subscription's range for the asked run `asked` starts past `[start, end)`, which it will
	 * not deliver while that range is live.
	 */
	skip(asked: Run, start: bigint, end: bigint): void {
		if (start >= end) return;
		this.#skipped = [...removeRun(this.#skipped, start, end), { start, end, asked: { ...asked } }].sort(byStart);
		this.#bump();
	}

	/**
	 * Settle each held fill against what the subscription refused, open a recovery for each run it
	 * will not deliver, and recompute what to ask. `tail` is the subscription's, and `read` whether
	 * anyone still reads the track. Returns whether the ask changed, or whether it {@link isEmpty}:
	 * a subscription holding its FIN for the fills waits on either.
	 */
	poll(tail: Tail, read: boolean): boolean {
		const empty = this.isEmpty();
		// The groups the subscription already accounted for, which it never sends again, are
		// recovered on their own, once.
		const asked: Run[] = [];
		for (const fill of this.#held) {
			const run = nextRun(fill);
			if (run) insertRun(asked, run.start, run.end);
		}
		for (const run of asked) {
			for (const gone of tail.accounted(run.start, run.end)) {
				for (const gap of runGaps(this.#recovering, gone.start, gone.end)) {
					const done = clip(this.#recovered, gap.start, gap.end);
					for (const again of done) {
						// Recovered once for these fills; a later request may recover it again.
						if (!this.#refusal(again.start))
							this.#setRefused(again.start, again.end, passing(new NotFound(`group ${again.start}`)));
					}
					for (const fresh of runGaps(done, gap.start, gap.end)) {
						if (!this.#refusal(fresh.start)) insertRun(this.#toRecover, fresh.start, fresh.end);
					}
				}
			}
		}
		// Each stale run goes out on its own subscription with no max delay, once, with the priority
		// of the fills held now: the last ask may predate them.
		let priority = this.ask.priority;
		for (const fill of this.#held) priority = Math.max(priority, fill.priority);
		for (const run of this.#toRecover.splice(0)) {
			insertRun(this.#recovered, run.start, run.end);
			insertRun(this.#recovering, run.start, run.end);
			this.#recoveries.add(new Recovery(run, this.#recover(run, priority), () => this.#bump()));
		}
		// What a finished recovery did not deliver stands refused.
		for (const done of this.#recoveries) {
			if (!done.refused) continue;
			this.#recoveries.delete(done);
			this.#recovering = removeRun(this.#recovering, done.groups.start, done.groups.end);
			for (const refused of done.refused) this.#setRefused(refused.start, refused.end, refused.refusal);
		}
		// What a recovery still out refused since the last poll answers the fills owing it now,
		// rather than waiting on its other streams. Taken once, so once those fills end, a later
		// request for the same groups asks again rather than reading it back.
		for (const recovery of this.#recoveries) {
			for (const refused of recovery.handle.takeRefused()) {
				this.#setRefused(refused.start, refused.end, refused.refusal);
				insertRun(recovery.released, refused.start, refused.end);
				this.#recovering = removeRun(this.#recovering, refused.start, refused.end);
			}
		}

		const ask: FillAsk = { groups: [], priority: NO_PRIORITY };
		// Every run a held fill still owes, recovered or not.
		const owed: Run[] = [];
		const held: track.Fill[] = [];
		for (const fill of this.#held) {
			for (;;) {
				const run = nextRun(fill);
				if (!run) {
					fill.close();
					break;
				}
				// The run extends from the group owed first, which the subscription may have refused
				// already. Refusing the run answers it, so the loop moves on.
				const first = fill.order === "ascending" ? run.start : run.end - 1n;
				const refused = this.#refusal(first);
				if (refused) {
					answer(refused.refusal, fill, max(run.start, refused.start), min(run.end, refused.end));
					continue;
				}
				insertRun(owed, run.start, run.end);
				// A run being recovered waits on its own subscription.
				for (const gap of runGaps(this.#recovering, run.start, run.end))
					insertRun(ask.groups, gap.start, gap.end);
				ask.priority = Math.max(ask.priority, fill.priority);
				held.push(fill);
				break;
			}
		}
		this.#held = held;

		// A refusal answers only the fills owing it now: once none covers its groups, a later request
		// asks again, and a loss it named stays recorded on the track alone.
		this.#refused = this.#refused.filter((run) =>
			held.some((fill) => fill.start < run.end && run.start < fill.end),
		);

		// A recovery no held fill owes any group of is cancelled, once none of its streams is still
		// being read or nobody reads the track, and a group is recovered once for the fills owing it
		// now: a later request for it may recover it again.
		for (const recovery of this.#recoveries) {
			const { start, end } = recovery.groups;
			if (clip(owed, start, end).length > 0 || (read && recovery.handle.reading())) continue;
			recovery.handle.cancel();
			this.#recoveries.delete(recovery);
		}
		this.#recovering = [];
		// Each covers its groups less what it refused and handed over.
		for (const { groups, released } of this.#recoveries)
			for (const gap of runGaps(released, groups.start, groups.end))
				insertRun(this.#recovering, gap.start, gap.end);
		this.#recovered = this.#recovered.flatMap(({ start, end }) => clip(owed, start, end));

		// A run stays asked whole while a fill still owes any of it, so the subscription changes once
		// as a run is asked and once as it is done, and the streams of its answered groups keep
		// arriving. One no fill owes stays while any of its streams is still arriving, each group
		// searched for once.
		for (const [sequence, closed] of this.#reading) if (closed.peek() !== undefined) this.#reading.delete(sequence);
		const groups = ask.groups.map((run) => ({ ...run }));
		const draining: Run[] = [];
		for (const run of this.ask.groups) {
			if (clip(ask.groups, run.start, run.end).length > 0) {
				insertRun(groups, run.start, run.end);
				continue;
			}
			// Only the groups the cache holds, so a wide run costs what it holds.
			for (const gap of runGaps(this.#draining, run.start, run.end)) {
				for (const sequence of hooks.arrivingIn(this.#copy, gap.start, gap.end)) {
					// Only watched until it ends, never taken over, so it is not held.
					const arriving = hooks.arrivingGroup(this.#copy, sequence);
					arriving?.release();
					if (arriving !== undefined) this.#reading.set(sequence, arriving.settled);
				}
			}
			if ([...this.#reading.keys()].some((sequence) => run.start <= sequence && sequence < run.end)) {
				insertRun(groups, run.start, run.end);
				insertRun(draining, run.start, run.end);
			}
		}
		for (const sequence of this.#reading.keys())
			if (!draining.some((run) => run.start <= sequence && sequence < run.end)) this.#reading.delete(sequence);
		this.#draining = draining;
		// What finishes a stream still asks with what its fills asked.
		if (this.#held.length === 0 && groups.length > 0) ask.priority = this.ask.priority;
		ask.groups = groups;
		if (ask.priority === this.ask.priority && sameRuns(ask.groups, this.ask.groups))
			return this.isEmpty() !== empty;
		this.#retireSkipped(ask.groups);
		this.ask = ask;
		return true;
	}

	/** Resolves once any input {@link poll} reads may have changed. */
	changed(tail: Tail): Promise<unknown> {
		const wakes: unknown[] = [
			Signal.race(
				this.#bumped,
				tail.changed,
				...[...this.#recoveries].map((recovery) => recovery.handle.changed),
			),
		];
		for (const fill of this.#held) wakes.push(fillChanged(fill));
		for (const closed of this.#reading.values()) wakes.push(closed);
		return race(wakes);
	}

	/** The copy closed: every held fill names what is left dropped, and every recovery stops. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const fill of this.#held.splice(0)) fill.close();
		for (const recovery of this.#recoveries) recovery.handle.cancel();
		this.#recoveries.clear();
		this.ask = { groups: [], priority: NO_PRIORITY };
		this.#bump();
	}

	/** Whether {@link close} was called. */
	get closed(): boolean {
		return this.#closed;
	}

	// Why the subscription or a recovery will not deliver the run of groups from `first`, if it
	// said so.
	#refusal(first: bigint): Refused | undefined {
		const refused = this.#refused.find((run) => run.start <= first && first < run.end);
		if (refused) return refused;
		// The subscriber's own range starting past them says nothing of the source.
		const skipped = this.#skipped.find((run) => run.start <= first && first < run.end);
		if (!skipped) return undefined;
		return { start: skipped.start, end: skipped.end, refusal: passing(new NotFound(`group ${skipped.start}`)) };
	}

	#setRefused(start: bigint, end: bigint, refusal: Refusal): void {
		this.#refused = refuseRun(this.#refused, start, end, refusal);
	}

	// Asking for `asked` from now on retires the range of each run not asked whole any more, and
	// what its start skipped with it.
	#retireSkipped(asked: readonly Run[]): void {
		this.#skipped = this.#skipped.filter((skipped) =>
			asked.some((run) => run.start === skipped.asked.start && run.end === skipped.asked.end),
		);
	}

	#bump(): void {
		this.#bumped.update((revision) => revision + 1);
	}
}

// The run `fill` owes first in its order, extending from the group it owes first.
function nextRun(fill: track.Fill): Run | undefined {
	const owed = wantedRuns(fill);
	return fill.order === "ascending" ? owed[0] : owed.at(-1);
}

// Answer `[start, end)` of `fill`: a lost group is named dropped, as a FETCH's miss is, and
// anything else is refused for now, recorded nowhere.
function answer(refusal: Refusal, fill: track.Fill, start: bigint, end: bigint): void {
	if (refusal.lost) fill.dropGroups(start, end);
	else fill.refuseGroups(start, end, refusal.error);
}

// The parts of `runs` inside `[start, end)`.
function clip(runs: readonly Run[], start: bigint, end: bigint): Run[] {
	const parts: Run[] = [];
	for (const run of runs) {
		const from = max(run.start, start);
		const to = min(run.end, end);
		if (from < to) parts.push({ start: from, end: to });
	}
	return parts;
}

function sameRuns(a: readonly Run[], b: readonly Run[]): boolean {
	return a.length === b.length && a.every((run, i) => run.start === b[i].start && run.end === b[i].end);
}

function byStart(a: Run, b: Run): number {
	return a.start < b.start ? -1 : a.start > b.start ? 1 : 0;
}
