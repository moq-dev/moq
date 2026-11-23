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
import { Ranges } from "./ranges.ts";
import type { Order, Range, Subscription } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import { isEmpty, type Joining, joinOf, maxOf, rangeBudget, rangeKey, sameRanges } from "./window.ts";

/** The preferences besides the ranges, already checked and defaulted. */
export interface Scalars {
	priority: number;
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
	/** The group each live join among `add` was already placed at, by its key: see {@link Subscription.placed}. */
	placed?: ReadonlyMap<bigint, bigint>;
}

/** A range as a subscriber holds it, with the group it was {@link Subscription.placed | placed} at. Mirrors the Rust `Held`. */
export interface Held {
	range: Range;
	placed?: bigint;
}

// A held range as a string, equal exactly when the ranges and their placements are.
function heldKey(held: Held): string {
	return held.placed === undefined ? rangeKey(held.range) : `${rangeKey(held.range)}@${held.placed}`;
}

// `ranges` held with the groups `placed` maps their indexes to.
function heldOf(ranges: readonly Range[], placed: ReadonlyMap<number, bigint> | undefined): Held[] {
	return ranges.map((range, index) => {
		const group = placed?.get(index);
		return group === undefined ? { range } : { range, placed: group };
	});
}

/** Whether two placements by index agree, none being the same as an empty one. */
export function samePlaced(
	a: ReadonlyMap<number, bigint> | undefined,
	b: ReadonlyMap<number, bigint> | undefined,
): boolean {
	if ((a?.size ?? 0) !== (b?.size ?? 0)) return false;
	for (const [index, group] of a ?? []) if (b?.get(index) !== group) return false;
	return true;
}

/** What one subscriber adds to an aggregate besides its ranges. */
export interface Contribution {
	priority: number;
	descending: boolean;
}

/**
 * A live join a received copy's readers wait on, by its lag in milliseconds and its end, with the
 * widest max in milliseconds one waits with, as a budget, and the newest answer tick any of them saw
 * before it began waiting: only an answer recorded after that is fresh for every one of them.
 * Mirrors the Rust `JoinAsk`.
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
	gone: readonly Held[];
	/** The ranges `after` holds and `before` doesn't, counted. */
	came: readonly Held[];
}

function contribution(scalars: Scalars): Contribution {
	return { priority: scalars.priority, descending: scalars.order === "descending" };
}

function sameScalars(a: Scalars, b: Scalars): boolean {
	return a.priority === b.priority && a.order === b.order;
}

// Whether `next` holds `old`'s ranges in order, each the same or the same live join under another
// max: a change that keeps every key, so each join stays where it joined.
function retunes(old: readonly Range[], next: readonly Range[]): boolean {
	return (
		old.length === next.length &&
		old.every((range, index) => {
			const join = joinOf(next[index]);
			return rangeKey(range) === rangeKey(next[index]) || (join !== undefined && joinOf(range) === join);
		})
	);
}

/** The ranges `old` holds more of than `next`, then those `next` holds more of than `old`. */
function moved(old: readonly Held[], next: readonly Held[]): [Held[], Held[]] {
	const counts = new Map<string, { held: Held; count: number }>();
	const tally = (held: Held, delta: number) => {
		const key = heldKey(held);
		const counted = counts.get(key);
		if (counted) counted.count += delta;
		else counts.set(key, { held, count: delta });
	};
	for (const held of old) tally(held, 1);
	for (const held of next) tally(held, -1);
	const gone: Held[] = [];
	const came: Held[] = [];
	for (const { held, count } of counts.values()) {
		for (let left = Math.abs(count); left > 0; left--) (count > 0 ? gone : came).push(held);
	}
	return [gone, came];
}

// How many answers to a received copy's live joins were recorded, on any track: a live join range a
// reader adds notes it, and takes only an answer recorded since (see {@link Prefs.since}). Counted
// process-wide, so a front's reader and the copies it reads compare their ticks. Mirrors the Rust
// `JOIN_TICK`.
let JOIN_TICK = 0;

/** How many join answers were recorded: see {@link answered}. Mirrors the Rust `join_tick`. */
export function joinTick(): number {
	return JOIN_TICK;
}

/**
 * Note an answer to a live join recorded, returning its tick: greater than that of every range
 * added before. Mirrors the Rust `answered`.
 */
export function answered(): number {
	return ++JOIN_TICK;
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
	// The group each live join range was already placed at, by key: see {@link Subscription.placed}.
	#placed = new Map<bigint, bigint>();
	// The preferences as last read, until they change.
	#value?: Subscription;
	/** Bumped on every change, so a reader wakes without reading the ranges. */
	readonly revision = new Signal(0);
	/** Told each change, so the aggregates this subscriber joins move by what changed. */
	watch?: (change: Change) => void;
	/**
	 * Whether the ranges last changed by a retune alone, each live join kept under its key with
	 * another max, rather than replaced or restated.
	 */
	retuned = false;

	/** Start with `subscription`, already checked and defaulted, or none. */
	constructor(subscription?: Subscription) {
		if (subscription) this.#replace(subscription);
	}

	/** The preferences as a {@link Subscription}, costing every range once per change. */
	peek(): Subscription | undefined {
		if (!this.#scalars) return undefined;
		if (!this.#value) {
			this.#value = { ...this.#scalars, ranges: [...this.#ranges.values()] };
			const placed = this.#placedByIndex();
			if (placed.size > 0) this.#value.placed = placed;
		}
		return this.#value;
	}

	// The group each placed join was placed at, by its index in key order, as
	// {@link Subscription.placed} holds it. Mirrors the Rust `Prefs::placed_by_index`.
	#placedByIndex(): Map<number, bigint> {
		const placed = new Map<number, bigint>();
		if (this.#placed.size === 0) return placed;
		for (const [index, key] of [...this.#ranges.keys()].entries()) {
			const group = this.#placed.get(key);
			if (group !== undefined) placed.set(index, group);
		}
		return placed;
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

	/** Every range in key order, with the group each was placed at. Mirrors the Rust `Prefs::held`. */
	held(): Held[] {
		return Array.from(this.#ranges, ([key, range]) => {
			const placed = this.#placed.get(key);
			return placed === undefined ? { range } : { range, placed };
		});
	}

	/** The group the live join range keyed `key` was placed at, if it was. Mirrors the Rust `Prefs::placed`. */
	placed(key: bigint): bigint | undefined {
		return this.#placed.get(key);
	}

	/**
	 * How a received copy places the live join range keyed `key`: where it was already placed, or
	 * from an answer recorded since it was added. Mirrors the Rust `Prefs::joining`.
	 */
	joining(key: bigint): Joining {
		const at = this.#placed.get(key);
		return at === undefined ? { since: this.since(key) } : { at };
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
	 * Note each live join range keyed in `since` as added at its tick instead, such as the one a
	 * front's reader added it at, which the copy it reads asked for: an answer recorded since then
	 * places it. Other ranges are left as they are. Returns the keys whose tick moved. Mirrors the
	 * Rust `Preferences::stamp`.
	 */
	stamp(since: Iterable<readonly [bigint, number]>): bigint[] {
		const moved: bigint[] = [];
		for (const [key, tick] of since) {
			const held = this.#since.get(key);
			if (held === undefined || held === tick || this.#placed.has(key)) continue;
			this.#since.set(key, tick);
			moved.push(key);
		}
		return moved;
	}

	/** Wake every reader, as a change would, without changing anything. */
	touch(): void {
		this.revision.update((revision) => revision + 1);
	}

	/**
	 * Replace every preference, already checked and defaulted, rekeying the ranges by position when
	 * they change. Ranges that change only a live join's max keep their keys, so each stays where it
	 * joined, unless `retune` is false, as a subscription restated whole: those come back as the keyed
	 * ranges they now read. Returns whether the ranges changed, and how; nothing happens unless
	 * something did. Mirrors the Rust `Prefs::replace`.
	 */
	set(next: Subscription, retune = true): false | "replaced" | (readonly [bigint, Range])[] {
		const scalars = this.#scalars;
		const old = [...this.#ranges.values()];
		const placed = samePlaced(this.#placedByIndex(), next.placed);
		const same = placed && sameRanges(old, next.ranges ?? []);
		if (scalars && same && sameScalars(scalars, next as Scalars)) return false;
		const before = this.contribution();
		const [gone, came] = same ? [[], []] : moved(this.held(), heldOf(next.ranges ?? [], next.placed));
		const retuned =
			!same && retune && placed && retunes(old, next.ranges ?? []) ? this.#retune(next.ranges ?? []) : undefined;
		if (!same) this.retuned = retuned !== undefined;
		this.#replace(next, retuned !== undefined);
		this.watch?.({ before, after: this.contribution(), gone, came });
		this.touch();
		return same ? false : (retuned ?? "replaced");
	}

	// Replace each range that changed only its live join's max under its own key, keeping its join
	// tick. Returns the ranges replaced, with their keys.
	#retune(next: readonly Range[]): (readonly [bigint, Range])[] {
		const changed: (readonly [bigint, Range])[] = [];
		for (const [index, [key, old]] of [...this.#ranges].entries()) {
			const range = next[index];
			if (rangeKey(old) === rangeKey(range)) continue;
			this.#ranges.set(key, range);
			changed.push([key, range]);
		}
		return changed;
	}

	/** Apply `edit`, costing the ranges it names. Returns whether the ranges changed. */
	edit(edit: Edit): boolean {
		const before = this.contribution();
		const { retire, add, placed, ...scalars } = edit;
		const readded = new Set(add.map(([key]) => key));
		const gone: Held[] = [];
		const came: Held[] = [];
		const tick = joinTick();
		const held = (key: bigint, range: Range): Held => {
			const placed = this.#placed.get(key);
			return placed === undefined ? { range } : { range, placed };
		};
		for (const key of retire) {
			const range = this.#ranges.get(key);
			// One added again is replaced in place below, keeping its order.
			if (range === undefined || readded.has(key)) continue;
			gone.push(held(key, range));
			this.#ranges.delete(key);
			this.#since.delete(key);
			this.#placed.delete(key);
		}
		for (const [key, range] of add) {
			const old = this.#ranges.get(key);
			if (old !== undefined) gone.push(held(key, old));
			this.#ranges.set(key, range);
			const group = placed?.get(key);
			if (group === undefined) this.#placed.delete(key);
			else this.#placed.set(key, group);
			// A live join that changes only its max keeps the tick it was added at, since it stays where
			// it joined. Mirrors the Rust `Prefs::tick_for`.
			const join = joinOf(range);
			if (join === undefined) this.#since.delete(key);
			else if (old === undefined || joinOf(old) !== join) this.#since.set(key, tick);
			came.push(held(key, range));
		}
		const moved = gone.length > 0 || came.length > 0;
		if (!moved && this.#scalars && sameScalars(this.#scalars, scalars)) return false;
		this.#scalars = scalars;
		this.#value = undefined;
		this.watch?.({ before, after: this.contribution(), gone, came });
		this.touch();
		return moved;
	}

	// Take `next`, already checked and defaulted, rekeying its ranges by position unless they were
	// `retuned` in place already.
	#replace(next: Subscription, retuned = false): void {
		const { ranges, placed, ...rest } = next;
		this.#scalars = rest as Scalars;
		if (
			!retuned &&
			(!sameRanges([...this.#ranges.values()], ranges ?? []) || !samePlaced(this.#placedByIndex(), placed))
		) {
			this.#ranges = new Map((ranges ?? []).map((range, index) => [BigInt(index), range]));
			const tick = joinTick();
			this.#since.clear();
			this.#placed.clear();
			for (const [key, range] of this.#ranges) {
				if (joinOf(range) !== undefined) this.#since.set(key, tick);
				const group = placed?.get(Number(key));
				if (group !== undefined) this.#placed.set(key, group);
			}
		}
		// Read back as given, so its identity holds until the next change.
		this.#value = next;
	}
}

/**
 * The aggregate of every subscriber's preferences, counted so a change costs what it moved:
 * the highest priority and widest staleness budget, descending if any subscriber is, and the union
 * of the ranges. Read as a {@link Subscription}, materialized only when read. Mirrors the Rust
 * `Subscriptions`.
 */
export class Subscriptions implements Getter<Subscription | undefined> {
	#count = 0;
	#priority = new Counts<number>();
	// Each non-empty range's staleness budget, counted: a history range's is the widest of all.
	#maxDelay = new Counts<number>();
	#descending = 0;
	#ranges = new Ranges();
	// The live joins already placed (see {@link Subscription.placed}), by the group each was placed at
	// and its end, with each one's staleness budget counted. Kept out of {@link #ranges}: a placed join
	// starts where it was placed, whatever its lag, so it merges with no other range.
	#placed = new Map<string, { group: bigint; end: bigint | undefined; budgets: Counts<number> }>();
	// The time starts a received copy's readers wait on, by time, counted: see {@link asks}.
	#asks = new Map<bigint, number>();
	// The live joins a received copy's readers wait on, by lag and end, each budget they wait under
	// and the answer tick each began waiting at counted: see {@link joins}.
	#joins = new Map<
		string,
		{ min: number; end: bigint | undefined; budgets: Counts<number>; since: Counts<number> }
	>();
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
		for (const { range } of came) if (!isEmpty(range)) this.#maxDelay.add(rangeBudget(range));
		for (const { range } of gone) if (!isEmpty(range)) this.#maxDelay.remove(rangeBudget(range));
		if (after) {
			this.#priority.add(after.priority);
			this.#descending += after.descending ? 1 : 0;
			this.#count++;
		}
		for (const held of came) if (this.#addRange(held)) reshaped = true;
		if (before) {
			this.#priority.remove(before.priority);
			this.#descending -= before.descending ? 1 : 0;
			this.#count--;
		}
		for (const held of gone) if (this.#removeRange(held)) reshaped = true;
		const next = this.#scalars();
		const changed = reshaped || prev?.priority !== next?.priority || prev?.descending !== next?.descending;
		if (changed) {
			this.#value = null;
			this.#revision.update((revision) => revision + 1);
		}
		return changed;
	}

	// Count `held` into the ranges, returning whether their aggregate moved. Mirrors the Rust
	// `Subscriptions::add_range`.
	#addRange({ range, placed }: Held): boolean {
		if (placed === undefined) return this.#ranges.add(range);
		const key = `${placed}:${range.end ?? ""}`;
		let held = this.#placed.get(key);
		if (!held) {
			held = { group: placed, end: range.end, budgets: new Counts<number>() };
			this.#placed.set(key, held);
		}
		const widest = held.budgets.last();
		held.budgets.add(rangeBudget(range));
		return widest !== held.budgets.last();
	}

	// Take back what {@link #addRange} counted, returning whether the aggregate moved. Mirrors the
	// Rust `Subscriptions::remove_range`.
	#removeRange({ range, placed }: Held): boolean {
		if (placed === undefined) return this.#ranges.remove(range);
		const key = `${placed}:${range.end ?? ""}`;
		const held = this.#placed.get(key);
		if (!held) throw new Error("a counted placed join");
		const widest = held.budgets.last();
		held.budgets.remove(rangeBudget(range));
		const after = held.budgets.last();
		if (after === undefined) this.#placed.delete(key);
		return widest !== after;
	}

	/**
	 * Count a reader waiting on time start `time` once more when `waits`, else once less. Returns
	 * whether {@link asks} changed, the first starting or the last stopping waiting on it, which wakes
	 * every watcher as any change to the aggregate does. Mirrors the Rust `Subscriptions::ask`.
	 */
	ask(time: bigint, waits: boolean): boolean {
		const held = this.#asks.get(time) ?? 0;
		if (!waits && held === 0) return false;
		const count = held + (waits ? 1 : -1);
		if (count === 0) this.#asks.delete(time);
		else this.#asks.set(time, count);
		const changed = waits ? held === 0 : count === 0;
		if (changed) this.#revision.update((revision) => revision + 1);
		return changed;
	}

	/**
	 * The time starts a received copy's readers wait on, lowest first: its session asks its
	 * upstream to place each with a range of its own. Mirrors the Rust `Subscription::asks`.
	 */
	asks(): bigint[] {
		return [...this.#asks.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
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
	 * The live joins a received copy's readers wait on, each with the widest max one waits with and
	 * the newest answer tick one saw before it began: its session asks its upstream to place each
	 * with a range of its own. Mirrors the Rust `Subscription::joins`.
	 */
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

	/** The widest staleness budget in milliseconds any range asks under, without reading the ranges. */
	maxDelay(): number {
		return this.#maxDelay.last() ?? 0;
	}

	#scalars(): Contribution | undefined {
		if (this.#count === 0) return undefined;
		return { priority: this.#priority.last() ?? 0, descending: this.#descending > 0 };
	}

	/** The aggregate, costing the union's length once per change. */
	peek(): Subscription | undefined {
		if (this.#value !== null) return this.#value;
		const scalars = this.#scalars();
		if (!scalars) {
			this.#value = undefined;
			return undefined;
		}
		const ranges = [...this.#ranges.union()];
		const value: Subscription = {
			priority: scalars.priority,
			// One live subscriber held behind another's backlog is worse than a backlog sent late.
			order: scalars.descending ? "descending" : "ascending",
			ranges,
		};
		// Each placed join once under the widest max asked of it, by group then end, the open one
		// last, as the Rust map orders them; its lag places nothing.
		if (this.#placed.size > 0) {
			const placed = new Map<number, bigint>();
			const end = (join: { end: bigint | undefined }) => join.end ?? UNBOUNDED;
			const order = (a: bigint, b: bigint) => (a < b ? -1 : a > b ? 1 : 0);
			const held = [...this.#placed.values()].sort((a, b) => order(a.group, b.group) || order(end(a), end(b)));
			for (const { group, end, budgets } of held) {
				placed.set(ranges.length, group);
				const join: Range = { start: { live: { min: Duration.ZERO, max: maxOf(budgets.last() ?? 0) } } };
				if (end !== undefined) join.end = end;
				ranges.push(join);
			}
			value.placed = placed;
		}
		this.#value = value;
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
