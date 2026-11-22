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
import { liveMillis, rangeKey, sameRanges } from "./window.ts";

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
	descending: boolean;
}

/**
 * A live join a received copy's readers wait on, by its lag in milliseconds and its end, with the
 * widest budget in milliseconds one waits under and the newest answer tick any of them saw before
 * it began waiting: only an answer recorded after that is fresh for every one of them. Mirrors the
 * Rust `JoinAsk`.
 */
export interface JoinAsk {
	min: number;
	end: bigint | undefined;
	budget: number;
	since: number;
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

function contribution(scalars: Scalars): Contribution {
	return {
		priority: scalars.priority,
		maxDelay: maxDelayOf(scalars),
		descending: scalars.order === "descending",
	};
}

function sameScalars(a: Scalars, b: Scalars): boolean {
	const same = (x: Duration | null | undefined, y: Duration | null | undefined) =>
		x === y || (x instanceof Duration && y instanceof Duration && x.eq(y));
	return a.priority === b.priority && a.order === b.order && same(a.delay.max, b.delay.max);
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
	// The join tick each live join range was added at, by key: see {@link since}.
	#since = new Map<bigint, number>();
	// The join tick a range added now is added at: the producer's {@link Subscriptions.joinTick}
	// once these join its aggregate, 0 before.
	#tick: () => number = () => 0;
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
		return this.#scalars && contribution(this.#scalars);
	}

	/** Every range, in key order. */
	ranges(): IterableIterator<Range> {
		return this.#ranges.values();
	}

	/** Every range with its key, in key order. */
	entries(): IterableIterator<[bigint, Range]> {
		return this.#ranges.entries();
	}

	/**
	 * The join tick the live join range keyed `key` was added at, 0 for another: a received copy
	 * places it only from an answer recorded since, the live edge having moved since any earlier one.
	 * Mirrors the Rust `Prefs::since`.
	 */
	since(key: bigint): number {
		return this.#since.get(key) ?? 0;
	}

	/**
	 * Add the ranges from now on at the join tick `tick` reads, and the ranges held at the current
	 * one: these join the aggregate that counts the answers, as the Rust `Preferences::register`
	 * reads its tick.
	 */
	ticking(tick: () => number): void {
		this.#tick = tick;
		this.#since.clear();
		const now = tick();
		for (const [key, range] of this.#ranges) if (liveMillis(range) !== undefined) this.#since.set(key, now);
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
		const tick = this.#tick();
		for (const key of retire) {
			const range = this.#ranges.get(key);
			// One added again is replaced in place below, keeping its order.
			if (range === undefined || readded.has(key)) continue;
			this.#ranges.delete(key);
			this.#since.delete(key);
			gone.push(range);
		}
		for (const [key, range] of add) {
			const old = this.#ranges.get(key);
			if (old !== undefined) gone.push(old);
			this.#ranges.set(key, range);
			if (liveMillis(range) !== undefined) this.#since.set(key, tick);
			else this.#since.delete(key);
			came.push(range);
		}
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
			const tick = this.#tick();
			this.#since.clear();
			for (const [key, range] of this.#ranges) if (liveMillis(range) !== undefined) this.#since.set(key, tick);
		}
		// Read back as given, so its identity holds until the next change.
		this.#value = next;
	}
}

/**
 * The aggregate of every subscriber's preferences, counted so a change costs what it moved:
 * the highest priority and max delay, descending if any subscriber is, and the union of the
 * ranges. Read as a {@link Subscription}, materialized only when read. Mirrors the Rust
 * `Subscriptions`.
 */
export class Subscriptions implements Getter<Subscription | undefined> {
	#count = 0;
	#priority = new Counts<number>();
	#maxDelay = new Counts<number>();
	#descending = 0;
	#ranges = new Ranges();
	// The time starts a received copy's readers wait on, by time, each budget they wait under
	// counted: see {@link asks}.
	#asks = new Map<bigint, Counts<number>>();
	// The live joins a received copy's readers wait on, by lag and end, each budget they wait under
	// and the answer tick each began waiting at counted: see {@link joins}.
	#joins = new Map<
		string,
		{ min: number; end: bigint | undefined; budgets: Counts<number>; since: Counts<number> }
	>();
	// How many answers to a received copy's live joins were recorded: a live join range a reader
	// adds notes it, and takes only an answer recorded since (see {@link Prefs.since}).
	#joinTick = 0;
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
			this.#descending += after.descending ? 1 : 0;
			this.#count++;
		}
		for (const range of came) if (this.#ranges.add(range)) reshaped = true;
		if (before) {
			this.#priority.remove(before.priority);
			this.#maxDelay.remove(before.maxDelay);
			this.#descending -= before.descending ? 1 : 0;
			this.#count--;
		}
		for (const range of gone) if (this.#ranges.remove(range)) reshaped = true;
		const next = this.#scalars();
		const changed =
			reshaped ||
			prev?.priority !== next?.priority ||
			prev?.maxDelay !== next?.maxDelay ||
			prev?.descending !== next?.descending;
		if (changed) {
			this.#value = null;
			this.#revision.update((revision) => revision + 1);
		}
		return changed;
	}

	/**
	 * Count a reader waiting on time start `time` under `budget`, the max delay in milliseconds,
	 * once more when `waits`, else once less. Returns whether {@link asks} changed, which wakes
	 * every watcher as any change to the aggregate does. Mirrors the Rust `Subscriptions::ask`.
	 */
	ask(time: bigint, budget: number, waits: boolean): boolean {
		let budgets = this.#asks.get(time);
		const widest = budgets?.last();
		if (waits) {
			if (!budgets) {
				budgets = new Counts<number>();
				this.#asks.set(time, budgets);
			}
			budgets.add(budget);
		} else if (budgets) {
			budgets.remove(budget);
			if (budgets.last() === undefined) this.#asks.delete(time);
		}
		const changed = this.#asks.get(time)?.last() !== widest;
		if (changed) this.#revision.update((revision) => revision + 1);
		return changed;
	}

	/**
	 * The time starts a received copy's readers wait on, lowest first, each with the widest budget
	 * in milliseconds one waits under: its session asks its upstream to place each with a range of
	 * its own. Mirrors the Rust `Subscription::asks`.
	 */
	asks(): [bigint, number][] {
		const asks: [bigint, number][] = [];
		for (const [time, budgets] of this.#asks) {
			const widest = budgets.last();
			if (widest !== undefined) asks.push([time, widest]);
		}
		return asks.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
	}

	/**
	 * Count a reader waiting on `join` once more when `waits`, else once less. Returns whether
	 * {@link joins} changed, which wakes every watcher as any change to the aggregate does. Mirrors
	 * the Rust `Subscriptions::join`.
	 */
	join(join: JoinAsk, waits: boolean): boolean {
		const key = `${join.min}:${join.end ?? ""}`;
		let waiting = this.#joins.get(key);
		const before = waiting && [waiting.budgets.last(), waiting.since.last()];
		if (waits) {
			if (!waiting) {
				waiting = { min: join.min, end: join.end, budgets: new Counts<number>(), since: new Counts<number>() };
				this.#joins.set(key, waiting);
			}
			waiting.budgets.add(join.budget);
			waiting.since.add(join.since);
		} else if (waiting) {
			waiting.budgets.remove(join.budget);
			waiting.since.remove(join.since);
			if (waiting.budgets.last() === undefined) this.#joins.delete(key);
		}
		const held = this.#joins.get(key);
		const after = held && [held.budgets.last(), held.since.last()];
		const changed = before?.[0] !== after?.[0] || before?.[1] !== after?.[1];
		if (changed) this.#revision.update((revision) => revision + 1);
		return changed;
	}

	/**
	 * The live joins a received copy's readers wait on, each with the widest budget one waits under
	 * and the newest answer tick one saw before it began: its session asks its upstream to place each
	 * with a range of its own. Mirrors the Rust `Subscription::joins`.
	 */
	/** How many join answers were recorded: see {@link answered}. Mirrors the Rust `Subscriptions::join_tick`. */
	joinTick(): number {
		return this.#joinTick;
	}

	/**
	 * Note an answer to a live join recorded, returning its tick: greater than that of every range
	 * added before. Mirrors the Rust `Subscriptions::answered`.
	 */
	answered(): number {
		return ++this.#joinTick;
	}

	joins(): JoinAsk[] {
		const joins: JoinAsk[] = [];
		for (const { min, end, budgets, since } of this.#joins.values()) {
			const budget = budgets.last();
			const tick = since.last();
			if (budget !== undefined && tick !== undefined) joins.push({ min, end, budget, since: tick });
		}
		// By lag, then end, the open one first, as the Rust map orders them.
		const end = (join: JoinAsk) => (join.end === undefined ? -1n : join.end);
		return joins.sort((a, b) => a.min - b.min || (end(a) < end(b) ? -1 : end(a) > end(b) ? 1 : 0));
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
			descending: this.#descending > 0,
		};
	}

	/** The aggregate, costing the union's length once per change. */
	peek(): Subscription | undefined {
		if (this.#value !== null) return this.#value;
		const scalars = this.#scalars();
		this.#value = scalars && {
			priority: scalars.priority,
			delay: { max: Duration.millis(scalars.maxDelay) },
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
