/**
 * The union of every subscriber's ranges, kept current per change rather than refolded. Mirrors
 * the Rust `demand::Ranges`.
 *
 * Time ranges count as runs of group starts in a {@link Cover}, so a change touches two boundaries
 * and asks one range-minimum question, whatever else the audience holds. A start between groups
 * stands for the group containing it, which a range starting at or before it covers too, so spans
 * merge as they do over starts. Ranges with no start can't be placed on that line and are counted
 * by value instead.
 *
 * @module
 */

import { Cover } from "./cover.ts";
import type { Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import { compare, isEmpty, rangeKey } from "./window.ts";

/** How {@link Ranges} counts one range. */
type Shape =
	| { kind: "empty" }
	/** The open range with no start. */
	| { kind: "open" }
	/** A time range, as the exclusive span of starts it covers, {@link UNBOUNDED} when open. */
	| { kind: "time"; start: bigint; end: bigint }
	/** A range with no start and an end. */
	| { kind: "until" };

function shapeOf(range: Range): Shape {
	if (isEmpty(range)) return { kind: "empty" };
	const { start, end } = range;
	if (start === undefined) return end === undefined ? { kind: "open" } : { kind: "until" };
	return { kind: "time", start, end: end === undefined ? UNBOUNDED : end + 1n };
}

/**
 * Every subscriber's ranges, counted so that one change costs O(log n), folded as the window
 * module's `union` folds them.
 */
export class Ranges {
	// Holders of the open range with no start.
	#open = 0;
	// Time ranges, as spans of starts.
	#time = new Cover();
	// Ranges with no start and an end, kept as they are.
	#until = new Map<string, { range: Range; count: number }>();
	// Whether a range with no start is a live join, which reaches back to no time span.
	#joins = false;
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
				changed = uncovered && !this.#covers();
				break;
			}
			case "until": {
				const key = rangeKey(range);
				const held = this.#until.get(key);
				if (held) held.count++;
				else this.#until.set(key, { range, count: 1 });
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
				changed = !this.#covers() && this.#time.least(shape.start, shape.end) <= 0;
				break;
			case "until": {
				const key = rangeKey(range);
				const held = this.#until.get(key);
				if (!held) break;
				const last = --held.count === 0;
				if (last) this.#until.delete(key);
				changed = last && this.#open === 0;
				break;
			}
		}
		return this.#moved(changed);
	}

	/**
	 * Set whether the ranges with no start are live joins (the aggregate Min Age is stated), which
	 * reach back to no time span, rather than reaching back to the oldest group. Returns whether
	 * the union changed.
	 */
	joins(joins: boolean): boolean {
		if (this.#joins === joins) return false;
		this.#joins = joins;
		return this.#moved(this.#open > 0);
	}

	// Whether the open range with no start reaches back to the oldest group, covering every other.
	#covers(): boolean {
		return this.#open > 0 && !this.#joins;
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
		if (this.#covers()) return [{}];
		const union: Range[] = this.#time
			.runs()
			.map((run) => (run.end === UNBOUNDED ? { start: run.start } : { start: run.start, end: run.end - 1n }));
		// Every range with no start begins at the same group, so the open one covers the rest.
		if (this.#open > 0) union.push({});
		else for (const { range } of this.#until.values()) union.push(range);
		return union.sort(compare);
	}
}
