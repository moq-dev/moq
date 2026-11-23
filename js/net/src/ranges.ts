/**
 * The union of every subscriber's ranges, kept current per change rather than refolded. Mirrors
 * the Rust `demand::Ranges`.
 *
 * Time ranges count as runs of group starts in a {@link Cover}, so a change touches two boundaries
 * and asks one range-minimum question, whatever else the audience holds. A start between groups
 * stands for the group containing it, which a range starting at or before it covers too, so spans
 * merge as they do over starts. Ranges with no time start can't be placed on that line and are
 * counted by value instead, live joins by lag and end under the largest max among them, since where
 * a join lands doesn't depend on it.
 *
 * @module
 */

import type { Duration } from "@moq/clock";
import { Counts, Cover } from "./cover.ts";
import type { Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import { compare, isEmpty, joinOf, maxOf, rangeBudget, rangeKey, rangeTime } from "./window.ts";

/** How {@link Ranges} counts one range. */
type Shape =
	| { kind: "empty" }
	/** From the oldest group, following the live edge. */
	| { kind: "open" }
	/** A time range, as the exclusive span of starts it covers, {@link UNBOUNDED} when open. */
	| { kind: "time"; start: bigint; end: bigint }
	/** A live join, by lag and end, and its budget. */
	| { kind: "join"; join: string; min: Duration; budget: number }
	/** The rest: a range from the oldest group with an end. */
	| { kind: "other" };

function shapeOf(range: Range): Shape {
	if (isEmpty(range)) return { kind: "empty" };
	const start = rangeTime(range);
	const end = range.end;
	if (start !== undefined) return { kind: "time", start, end: end === undefined ? UNBOUNDED : end + 1n };
	const join = joinOf(range);
	if (join !== undefined && typeof range.start === "object")
		return { kind: "join", join, min: range.start.live.min, budget: rangeBudget(range) };
	return range.start === undefined && end === undefined ? { kind: "open" } : { kind: "other" };
}

/**
 * Every subscriber's ranges, counted so that one change costs O(log n), folded as the window
 * module's `union` folds them.
 */
export class Ranges {
	// Holders of a range from the oldest group following the live edge, which covers every other.
	#open = 0;
	// Time ranges, as spans of starts.
	#time = new Cover();
	// Ranges from the oldest group with an end, kept as they are.
	#others = new Map<string, { range: Range; count: number }>();
	// Live joins by lag and end, each with the budgets its holders ask under, and the range asking
	// the widest.
	#joins = new Map<string, { budgets: Counts<number>; min: Duration; end: bigint | undefined }>();
	// The last union read, until a change moves it.
	#union?: Range[];

	/** Count `range`, returning whether the union changed. */
	add(range: Range): boolean {
		const shape = shapeOf(range);
		let changed = false;
		switch (shape.kind) {
			case "empty":
				break;
			case "open":
				changed = ++this.#open === 1;
				break;
			case "time": {
				const uncovered = this.#time.least(shape.start, shape.end) <= 0;
				this.#time.add(shape.start, shape.end, 1);
				changed = uncovered && this.#open === 0;
				break;
			}
			case "join": {
				let held = this.#joins.get(shape.join);
				if (!held) {
					held = { budgets: new Counts<number>(), min: shape.min, end: range.end };
					this.#joins.set(shape.join, held);
				}
				const widest = held.budgets.last();
				held.budgets.add(shape.budget);
				changed = (widest === undefined || shape.budget > widest) && this.#open === 0;
				break;
			}
			case "other": {
				const key = rangeKey(range);
				const held = this.#others.get(key);
				if (held) held.count++;
				else this.#others.set(key, { range, count: 1 });
				changed = !held && this.#open === 0;
				break;
			}
		}
		return this.#moved(changed);
	}

	/** Uncount `range`, one {@link add} counted, returning whether the union changed. */
	remove(range: Range): boolean {
		const shape = shapeOf(range);
		let changed = false;
		switch (shape.kind) {
			case "empty":
				break;
			case "open":
				changed = --this.#open === 0;
				break;
			case "time":
				this.#time.add(shape.start, shape.end, -1);
				changed = this.#open === 0 && this.#time.least(shape.start, shape.end) <= 0;
				break;
			case "join": {
				const held = this.#joins.get(shape.join);
				if (!held) break;
				const widest = held.budgets.last();
				held.budgets.remove(shape.budget);
				const now = held.budgets.last();
				if (now === undefined) this.#joins.delete(shape.join);
				changed = now !== widest && this.#open === 0;
				break;
			}
			case "other": {
				const key = rangeKey(range);
				const held = this.#others.get(key);
				if (!held) break;
				const last = --held.count === 0;
				if (last) this.#others.delete(key);
				changed = last && this.#open === 0;
				break;
			}
		}
		return this.#moved(changed);
	}

	// Drop the cached union when `changed`, passing it through.
	#moved(changed: boolean): boolean {
		if (changed) this.#union = undefined;
		return changed;
	}

	/** The union in a canonical order, costing its own length rather than the audience's. Read only. */
	union(): readonly Range[] {
		this.#union ??= this.#fold();
		return this.#union;
	}

	#fold(): Range[] {
		if (this.#open > 0) return [{}];
		const union: Range[] = this.#time
			.runs()
			.map((run) => (run.end === UNBOUNDED ? { start: run.start } : { start: run.start, end: run.end - 1n }));
		for (const { range } of this.#others.values()) union.push(range);
		for (const { budgets, min, end } of this.#joins.values()) {
			const widest = budgets.last();
			if (widest === undefined) continue;
			const range: Range = { start: { live: { min, max: maxOf(widest) } } };
			if (end !== undefined) range.end = end;
			union.push(range);
		}
		return union.sort(compare);
	}
}
