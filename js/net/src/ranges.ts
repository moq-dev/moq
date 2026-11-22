/**
 * The union of every subscriber's ranges, kept current per change rather than refolded. Mirrors
 * the Rust `demand::Ranges`.
 *
 * Time ranges count as runs of group starts in a {@link Cover}, so a change touches two boundaries
 * and asks one range-minimum question, whatever else the audience holds. A start between groups
 * stands for the group containing it, which a range starting at or before it covers too, so spans
 * merge as they do over starts. Ranges with no time start can't be placed on that line and are
 * counted by value instead.
 *
 * @module
 */

import { Cover } from "./cover.ts";
import type { Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import { compare, isEmpty, rangeKey, rangeTime } from "./window.ts";

/** How {@link Ranges} counts one range. */
type Shape =
	| { kind: "empty" }
	/** From the oldest group, following the live edge. */
	| { kind: "open" }
	/** A time range, as the exclusive span of starts it covers, {@link UNBOUNDED} when open. */
	| { kind: "time"; start: bigint; end: bigint }
	/** The rest: a live join, or a range from the oldest group with an end. */
	| { kind: "other" };

function shapeOf(range: Range): Shape {
	if (isEmpty(range)) return { kind: "empty" };
	const start = rangeTime(range);
	const end = range.end;
	if (start !== undefined) return { kind: "time", start, end: end === undefined ? UNBOUNDED : end + 1n };
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
	// The rest, live joins and ranges from the oldest group with an end, kept as they are.
	#others = new Map<string, { range: Range; count: number }>();
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
		return union.sort(compare);
	}
}
