/**
 * The union of every subscriber's ranges, kept current per change rather than refolded. Mirrors
 * the Rust `demand::Ranges`.
 *
 * Time ranges count as runs of group starts in a {@link Cover}, so a change touches two boundaries
 * and asks one range-minimum question, whatever else the audience holds. A start between groups
 * stands for the group containing it, which a range starting at or before it covers too, so spans
 * merge as they do over starts. Ranges anchored to the latest group can't be placed on that line
 * and are counted by value instead.
 *
 * @module
 */

import { Counts, Cover } from "./cover.ts";
import type { Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import { compare, isEmpty, rangeKey } from "./window.ts";

/** How {@link Ranges} counts one range. */
type Shape =
	| { kind: "empty" }
	| { kind: "oldest" }
	/** A time range, as the exclusive span of starts it covers, {@link UNBOUNDED} when open. */
	| { kind: "time"; start: bigint; end: bigint }
	| { kind: "relative"; back: bigint }
	| { kind: "mixed" };

function shapeOf(range: Range): Shape {
	if (isEmpty(range)) return { kind: "empty" };
	const { start, end } = range;
	if (start === undefined) return end === undefined ? { kind: "oldest" } : { kind: "mixed" };
	if (start.time !== undefined) {
		if (end === undefined) return { kind: "time", start: start.time, end: UNBOUNDED };
		// An end at the last start covers every group, as an open one does.
		if (end.time !== undefined) return { kind: "time", start: start.time, end: end.time + 1n };
		return { kind: "mixed" };
	}
	return end === undefined ? { kind: "relative", back: start.relative } : { kind: "mixed" };
}

/** Every subscriber's ranges, counted so that one change costs O(log n). */
export class Ranges {
	// Holders of the open oldest range, which covers every other range.
	#oldest = 0;
	// Time ranges, as spans of starts.
	#time = new Cover();
	// Open relative ranges by how far back they start: only the farthest matters.
	#relative = new Counts<bigint>();
	// Ranges with one relative end, or a bounded relative span, kept as they are.
	#mixed = new Map<string, { range: Range; count: number }>();
	// The last union read, until a change moves it.
	#union?: Range[];

	/** Count `range`, returning whether the union changed. */
	add(range: Range): boolean {
		const shape = shapeOf(range);
		let changed = false;
		switch (shape.kind) {
			case "empty":
				break;
			case "oldest":
				changed = ++this.#oldest === 1;
				break;
			case "time": {
				const uncovered = this.#time.least(shape.start, shape.end) <= 0;
				this.#time.add(shape.start, shape.end, 1);
				changed = uncovered && this.#oldest === 0;
				break;
			}
			case "relative": {
				const farthest = this.#relative.last();
				this.#relative.add(shape.back);
				changed = this.#oldest === 0 && this.#relative.last() !== farthest;
				break;
			}
			case "mixed": {
				const key = rangeKey(range);
				const held = this.#mixed.get(key);
				if (held) held.count++;
				else this.#mixed.set(key, { range, count: 1 });
				changed = !held && this.#oldest === 0 && !this.#covered(range);
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
			case "oldest":
				changed = --this.#oldest === 0;
				break;
			case "time":
				this.#time.add(shape.start, shape.end, -1);
				changed = this.#oldest === 0 && this.#time.least(shape.start, shape.end) <= 0;
				break;
			case "relative": {
				const farthest = this.#relative.last();
				this.#relative.remove(shape.back);
				changed = this.#oldest === 0 && this.#relative.last() !== farthest;
				break;
			}
			case "mixed": {
				const key = rangeKey(range);
				const held = this.#mixed.get(key);
				if (!held) break;
				const last = --held.count === 0;
				if (last) this.#mixed.delete(key);
				changed = last && this.#oldest === 0 && !this.#covered(range);
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
		if (this.#oldest > 0) return [{}];
		const union: Range[] = this.#time
			.runs()
			.map((run) =>
				run.end === UNBOUNDED
					? { start: { time: run.start } }
					: { start: { time: run.start }, end: { time: run.end - 1n } },
			);
		for (const { range } of this.#mixed.values()) if (!this.#covered(range)) union.push(range);
		const farthest = this.#relative.last();
		if (farthest !== undefined) union.push({ start: { relative: farthest } });
		return union.sort(compare);
	}

	// Whether an open range already delivers everything a mixed `range` could.
	#covered(range: Range): boolean {
		const start = range.start;
		// Only the open oldest range, which hides every other, covers the oldest group.
		if (start === undefined) return false;
		if (start.time !== undefined) {
			// The group containing a start at or before this one's comes no later.
			const tail = this.#time.openFrom();
			return tail !== undefined && tail <= start.time;
		}
		const farthest = this.#relative.last();
		return farthest !== undefined && start.relative <= farthest;
	}
}
