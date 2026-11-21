/**
 * Incremental demand: each subscriber's preferences keyed so an edit costs what it names, and the
 * aggregate across subscribers counted so a change costs what it moved. Mirrors the Rust
 * `track::demand`.
 *
 * @module
 */

import { Duration } from "@moq/clock";
import { type Dispose, type Getter, Signal } from "@moq/signals";
import { Counts } from "./cover.ts";
import { maxDelayOf } from "./internal.ts";
import { Ranges } from "./ranges.ts";
import type { Delay, Order, Range, Subscription } from "./track.ts";
import { rangeKey, sameRanges } from "./window.ts";

/** The preferences besides the ranges, already checked and defaulted. */
export interface Scalars {
	priority: number;
	delay: Delay;
	order: Order;
}

/**
 * A change to a subscriber's preferences that edits its ranges by key: the ranges keyed `retire`
 * go, then each of `add` joins under its key, and the rest stay as they resolved. A key added
 * again replaces its range in place; any other key added is past every key held. Costs the ranges
 * it names, however many the subscriber holds. Mirrors the Rust `Edit`.
 */
export interface Edit extends Scalars {
	retire: readonly bigint[];
	add: readonly (readonly [bigint, Range])[];
}

/** What one subscriber adds to an aggregate besides its ranges. */
export interface Contribution {
	priority: number;
	/** The max delay in milliseconds, see {@link maxDelayOf}. */
	maxDelay: number;
	/** The min delay in milliseconds, undefined when not stated. */
	minDelay?: number;
	/** Whether it holds a range with no start, the only kind its min delay places. */
	joins: boolean;
	descending: boolean;
}

/** A change to one subscriber's preferences, as an aggregate counts it. */
export interface Change {
	before?: Contribution;
	after?: Contribution;
	/** The ranges `before` held and `after` doesn't, counted. */
	gone: readonly Range[];
	/** The ranges `after` holds and `before` doesn't, counted. */
	came: readonly Range[];
}

function contribution(scalars: Scalars, joins: boolean): Contribution {
	return {
		priority: scalars.priority,
		maxDelay: maxDelayOf(scalars),
		minDelay: scalars.delay.min?.asMillis(),
		joins,
		descending: scalars.order === "descending",
	};
}

function sameScalars(a: Scalars, b: Scalars): boolean {
	const same = (x: Duration | null | undefined, y: Duration | null | undefined) =>
		x === y || (x instanceof Duration && y instanceof Duration && x.eq(y));
	return (
		a.priority === b.priority &&
		a.order === b.order &&
		same(a.delay.min, b.delay.min) &&
		same(a.delay.max, b.delay.max)
	);
}

// How many of `ranges` have no start.
function unstarted(ranges: readonly Range[]): number {
	return ranges.reduce((count, range) => count + (range.start === undefined ? 1 : 0), 0);
}

/** The ranges `old` holds more of than `next`, then those `next` holds more of than `old`. */
function moved(old: readonly Range[], next: readonly Range[]): [Range[], Range[]] {
	const counts = new Map<string, { range: Range; count: number }>();
	const tally = (range: Range, delta: number) => {
		const key = rangeKey(range);
		const held = counts.get(key);
		if (held) held.count += delta;
		else counts.set(key, { range, count: delta });
	};
	for (const range of old) tally(range, 1);
	for (const range of next) tally(range, -1);
	const gone: Range[] = [];
	const came: Range[] = [];
	for (const { range, count } of counts.values()) {
		for (let left = Math.abs(count); left > 0; left--) (count > 0 ? gone : came).push(range);
	}
	return [gone, came];
}

/**
 * A subscriber's preferences, the ranges keyed so an {@link Edit} costs only what it names. Reads
 * as the {@link Subscription} last given, its ranges in key order, materialized only when read.
 * Mirrors the Rust `Prefs`.
 */
export class Prefs implements Getter<Subscription | undefined> {
	#scalars?: Scalars;
	// In key order: a whole replacement keys by position, and each key an edit adds is past the rest.
	#ranges = new Map<bigint, Range>();
	// How many of the ranges have no start, which only the min delay places.
	#unstarted = 0;
	// The preferences as last read, until they change.
	#value?: Subscription;
	/** Bumped on every change, so a reader wakes without reading the ranges. */
	readonly revision = new Signal(0);
	/** Told each change, so the aggregates this subscriber joins move by what changed. */
	watch?: (change: Change) => void;

	/** Start with `subscription`, already checked and defaulted, or none. */
	constructor(subscription?: Subscription) {
		if (subscription) this.#replace(subscription);
	}

	/** The preferences as a {@link Subscription}, costing every range once per change. */
	peek(): Subscription | undefined {
		if (!this.#scalars) return undefined;
		this.#value ??= { ...this.#scalars, ranges: [...this.#ranges.values()] };
		return this.#value;
	}

	changed(): Promise<Subscription | undefined>;
	changed(fn: (value: Subscription | undefined) => void): Dispose;
	changed(fn?: (value: Subscription | undefined) => void): Promise<Subscription | undefined> | Dispose {
		if (!fn) return this.revision.changed().then(() => this.peek());
		return this.revision.changed(() => fn(this.peek()));
	}

	subscribe(fn: (value: Subscription | undefined) => void): Dispose {
		return this.revision.subscribe(() => fn(this.peek()));
	}

	/** The preferences besides the ranges, without reading the ranges. */
	scalars(): Scalars | undefined {
		return this.#scalars;
	}

	/** What these add to an aggregate besides the ranges. */
	contribution(): Contribution | undefined {
		return this.#scalars && contribution(this.#scalars, this.#unstarted > 0);
	}

	/** Every range, in key order. */
	ranges(): IterableIterator<Range> {
		return this.#ranges.values();
	}

	/** Every range with its key, in key order. */
	entries(): IterableIterator<[bigint, Range]> {
		return this.#ranges.entries();
	}

	/** Wake every reader, as a change would, without changing anything. */
	touch(): void {
		this.revision.update((revision) => revision + 1);
	}

	/**
	 * Replace every preference, already checked and defaulted, rekeying the ranges by position when
	 * they change. Returns whether the ranges changed; nothing happens unless something did.
	 */
	set(next: Subscription): boolean {
		const scalars = this.#scalars;
		const old = [...this.#ranges.values()];
		const same = sameRanges(old, next.ranges ?? []);
		if (scalars && same && sameScalars(scalars, next as Scalars)) return false;
		const before = this.contribution();
		const [gone, came] = same ? [[], []] : moved(old, next.ranges ?? []);
		this.#replace(next);
		this.watch?.({ before, after: this.contribution(), gone, came });
		this.touch();
		return !same;
	}

	/** Apply `edit`, costing the ranges it names. Returns whether the ranges changed. */
	edit(edit: Edit): boolean {
		const before = this.contribution();
		const { retire, add, ...scalars } = edit;
		const readded = new Set(add.map(([key]) => key));
		const gone: Range[] = [];
		const came: Range[] = [];
		for (const key of retire) {
			const range = this.#ranges.get(key);
			// One added again is replaced in place below, keeping its order.
			if (range === undefined || readded.has(key)) continue;
			this.#ranges.delete(key);
			gone.push(range);
		}
		for (const [key, range] of add) {
			const old = this.#ranges.get(key);
			if (old !== undefined) gone.push(old);
			this.#ranges.set(key, range);
			came.push(range);
		}
		this.#unstarted += unstarted(came) - unstarted(gone);
		const moved = gone.length > 0 || came.length > 0;
		if (!moved && this.#scalars && sameScalars(this.#scalars, scalars)) return false;
		this.#scalars = scalars;
		this.#value = undefined;
		this.watch?.({ before, after: this.contribution(), gone, came });
		this.touch();
		return moved;
	}

	#replace(next: Subscription): void {
		const { ranges, ...rest } = next;
		this.#scalars = rest as Scalars;
		if (!sameRanges([...this.#ranges.values()], ranges ?? [])) {
			this.#ranges = new Map((ranges ?? []).map((range, index) => [BigInt(index), range]));
			this.#unstarted = unstarted(ranges ?? []);
		}
		// Read back as given, so its identity holds until the next change.
		this.#value = next;
	}
}

/**
 * The aggregate of every subscriber's preferences, counted so a change costs what it moved:
 * the highest priority and max delay, the min delay, descending if any subscriber is, and the
 * union of the ranges.
 *
 * The min delay counts only the subscribers holding a range with no start, the only kind it
 * places: the largest of theirs, with one not stated counting as the largest, so a relay's copy
 * reaches back for the reader that buffers most, and an oldest reader still gets its backlog. Read as a {@link Subscription}, materialized only when read.
 * Mirrors the Rust `Subscriptions`.
 */
export class Subscriptions implements Getter<Subscription | undefined> {
	#count = 0;
	#priority = new Counts<number>();
	#maxDelay = new Counts<number>();
	// The stated minimums of the subscribers holding a range with no start.
	#minDelay = new Counts<number>();
	// How many subscribers hold a range with no start and state no minimum.
	#unstated = 0;
	#descending = 0;
	#ranges = new Ranges();
	#revision = new Signal(0);
	// The aggregate as last read, until it changes; null for none read yet.
	#value: Subscription | undefined | null = null;

	/**
	 * Move one subscriber from `before` to `after`, either undefined for none, its ranges by
	 * `gone` and `came`. Returns whether the aggregate changed.
	 */
	replace({ before, after, gone, came }: Change): boolean {
		const prev = this.#scalars();
		let reshaped = false;
		// New ranges count before old ones leave, so a range both hold never reads as gone.
		if (after) {
			this.#priority.add(after.priority);
			this.#maxDelay.add(after.maxDelay);
			if (after.joins && after.minDelay === undefined) this.#unstated++;
			else if (after.joins && after.minDelay !== undefined) this.#minDelay.add(after.minDelay);
			this.#descending += after.descending ? 1 : 0;
			this.#count++;
		}
		for (const range of came) if (this.#ranges.add(range)) reshaped = true;
		if (before) {
			this.#priority.remove(before.priority);
			this.#maxDelay.remove(before.maxDelay);
			if (before.joins && before.minDelay === undefined) this.#unstated--;
			else if (before.joins && before.minDelay !== undefined) this.#minDelay.remove(before.minDelay);
			this.#descending -= before.descending ? 1 : 0;
			this.#count--;
		}
		for (const range of gone) if (this.#ranges.remove(range)) reshaped = true;
		const next = this.#scalars();
		if (this.#ranges.joins(next?.minDelay !== undefined)) reshaped = true;
		const changed =
			reshaped ||
			prev?.priority !== next?.priority ||
			prev?.maxDelay !== next?.maxDelay ||
			prev?.minDelay !== next?.minDelay ||
			prev?.descending !== next?.descending;
		if (changed) {
			this.#value = null;
			this.#revision.update((revision) => revision + 1);
		}
		return changed;
	}

	/** The largest max delay in milliseconds, without reading the ranges. */
	maxDelay(): number {
		return this.#maxDelay.last() ?? 0;
	}

	#scalars(): Contribution | undefined {
		if (this.#count === 0) return undefined;
		return {
			priority: this.#priority.last() ?? 0,
			maxDelay: this.maxDelay(),
			minDelay: this.#unstated > 0 ? undefined : this.#minDelay.last(),
			joins: this.#unstated > 0 || this.#minDelay.last() !== undefined,
			descending: this.#descending > 0,
		};
	}

	/** The aggregate, costing the union's length once per change. */
	peek(): Subscription | undefined {
		if (this.#value !== null) return this.#value;
		const scalars = this.#scalars();
		this.#value = scalars && {
			priority: scalars.priority,
			delay: {
				max: Duration.millis(scalars.maxDelay),
				min: scalars.minDelay === undefined ? null : Duration.millis(scalars.minDelay),
			},
			// One live subscriber held behind another's backlog is worse than a backlog sent late.
			order: scalars.descending ? "descending" : "ascending",
			ranges: [...this.#ranges.union()],
		};
		return this.#value;
	}

	changed(): Promise<Subscription | undefined>;
	changed(fn: (value: Subscription | undefined) => void): Dispose;
	changed(fn?: (value: Subscription | undefined) => void): Promise<Subscription | undefined> | Dispose {
		if (!fn) return this.#revision.changed().then(() => this.peek());
		return this.#revision.changed(() => fn(this.peek()));
	}

	subscribe(fn: (value: Subscription | undefined) => void): Dispose {
		return this.#revision.subscribe(() => fn(this.peek()));
	}
}
