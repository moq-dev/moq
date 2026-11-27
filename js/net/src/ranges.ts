/**
 * The union of every subscriber's ranges, kept current per change rather than refolded. Mirrors
 * the Rust `demand::Ranges`.
 *
 * Time ranges count as runs of group starts in a {@link Cover}, so a change touches two boundaries
 * and asks one range-minimum question, whatever else the audience holds. A start between groups
 * stands for the group containing it, which a range starting at or before it covers too, so spans
 * merge as they do over starts. Ranges with no time start can't be placed on that line and are
 * counted by value instead: live joins by lag and end under the largest max among them, since where
 * a join lands doesn't depend on it, and ranges from the oldest group under the furthest end, which
 * takes in the rest.
 *
 * All that holds within one policy, since folding a range into one at another policy would lose its
 * priority or order. Across policies a range is dropped only once ranges at policies that
 * {@link covers | cover} its own cover its span. That fold is redone only when one policy's union
 * moves, at the cost of the union's own length.
 *
 * @module
 */

import type { Duration } from "@moq/clock";
import { Counts, Cover } from "./cover.ts";
import type { Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import {
	compare,
	isEmpty,
	joinOf,
	maxOf,
	policyOf,
	policyRank,
	rangeBudget,
	rangeKey,
	rangeTime,
	rankPolicy,
	withPolicy,
} from "./window.ts";

/** How {@link Ranges} counts one range. */
type Shape =
	| { kind: "empty" }
	/** From the oldest group, following the live edge. */
	| { kind: "open" }
	/** A time range, as the exclusive span of starts it covers, {@link UNBOUNDED} when open. */
	| { kind: "time"; start: bigint; end: bigint }
	/** A live join, by lag and end, and its budget. */
	| { kind: "join"; join: string; min: Duration | null; budget: number }
	/** A range from the oldest group, by its end. */
	| { kind: "oldest"; end: bigint };

function shapeOf(range: Range): Shape {
	if (isEmpty(range)) return { kind: "empty" };
	const start = rangeTime(range);
	const end = range.end;
	if (start !== undefined) return { kind: "time", start, end: end === undefined ? UNBOUNDED : end + 1n };
	const join = joinOf(range);
	if (join !== undefined && typeof range.start === "object")
		return { kind: "join", join, min: range.start.live.min, budget: rangeBudget(range) };
	if (end === undefined) return { kind: "open" };
	return { kind: "oldest", end };
}

/**
 * Every subscriber's ranges, counted so that one change costs O(log n) unless it moves the union of
 * its policy's ranges, folded as the window module's `union` folds them.
 */
export class Ranges {
	// The ranges of each policy, by its rank.
	#classes = new Map<number, Class>();
	// The union, refolded whenever one policy's union moves.
	#union: Range[] = [];

	/** Count `range`, returning whether the union changed. */
	add(range: Range): boolean {
		if (isEmpty(range)) return false;
		const rank = policyRank(policyOf(range));
		let held = this.#classes.get(rank);
		if (!held) {
			held = new Class();
			this.#classes.set(rank, held);
		}
		return held.add(range) && this.#refold();
	}

	/** Uncount `range`, one {@link add} counted, returning whether the union changed. */
	remove(range: Range): boolean {
		if (isEmpty(range)) return false;
		const rank = policyRank(policyOf(range));
		const held = this.#classes.get(rank);
		if (!held) throw new Error("a counted policy");
		const moved = held.remove(range);
		if (held.held === 0) this.#classes.delete(rank);
		return moved && this.#refold();
	}

	/** The union in a canonical order, costing its own length rather than the audience's. Read only. */
	union(): readonly Range[] {
		return this.#union;
	}

	// Fold the union again, returning whether it moved.
	#refold(): boolean {
		const above = new Above();
		const union: Range[] = [];
		// Those covering others first.
		for (const rank of [...this.#classes.keys()].sort((a, b) => b - a)) {
			const policy = rankPolicy(rank);
			const spans = (this.#classes.get(rank) as Class).union();
			for (const range of spans) if (!above.covers(range)) union.push(withPolicy(range, policy));
			above.extend(spans);
		}
		union.sort(compare);
		const moved =
			union.length !== this.#union.length ||
			union.some((range, index) => rangeKey(range) !== rangeKey(this.#union[index]));
		this.#union = union;
		return moved;
	}
}

/**
 * What ranges at covering policies already ask, as a range of one policy is checked against them:
 * the same spans {@link Class} merges ranges into.
 */
class Above {
	// Whether a range from the oldest group follows the live edge, covering everything.
	#all = false;
	// The time spans, merged where they overlap or touch, as exclusive spans of starts in order.
	#runs: { start: bigint; end: bigint }[] = [];
	// The furthest end of a range from the oldest group.
	#oldest: bigint | undefined;
	// Live joins by lag and end, under the widest budget.
	#joins = new Map<string, number>();

	covers(range: Range): boolean {
		if (this.#all) return true;
		const shape = shapeOf(range);
		switch (shape.kind) {
			case "empty":
				return true;
			case "open":
				return false;
			case "time": {
				// The last run starting at or before it, which alone can reach it.
				let low = 0;
				let high = this.#runs.length;
				while (low < high) {
					const mid = (low + high) >> 1;
					if (this.#runs[mid].start <= shape.start) low = mid + 1;
					else high = mid;
				}
				const run = this.#runs[low - 1];
				return run !== undefined && shape.end <= run.end;
			}
			case "join":
				return (this.#joins.get(shape.join) ?? -1) >= shape.budget;
			case "oldest":
				return this.#oldest !== undefined && this.#oldest >= shape.end;
		}
	}

	// Take in one policy's union.
	extend(ranges: readonly Range[]): void {
		const spans = [...this.#runs];
		for (const range of ranges) {
			const shape = shapeOf(range);
			switch (shape.kind) {
				case "empty":
					break;
				case "open":
					this.#all = true;
					break;
				case "time":
					spans.push({ start: shape.start, end: shape.end });
					break;
				case "join":
					this.#joins.set(shape.join, Math.max(this.#joins.get(shape.join) ?? -1, shape.budget));
					break;
				case "oldest":
					this.#oldest = this.#oldest === undefined || shape.end > this.#oldest ? shape.end : this.#oldest;
					break;
			}
		}
		spans.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));
		const runs: { start: bigint; end: bigint }[] = [];
		for (const span of spans) {
			const last = runs.at(-1);
			if (!last || span.start > last.end) runs.push({ ...span });
			else if (span.end > last.end) last.end = span.end;
		}
		this.#runs = runs;
	}
}

/** The ranges of one policy, counted so that one change costs O(log n). */
class Class {
	/** How many non-empty ranges are counted. */
	held = 0;
	// Holders of a range from the oldest group following the live edge, which covers every other.
	#open = 0;
	// Time ranges, as spans of starts.
	#time = new Cover();
	// Ranges from the oldest group with an end, by end: the furthest takes in the rest.
	#oldest = new Counts<bigint>();
	// Live joins by lag and end, each with the budgets its holders ask under, and the range asking
	// the widest.
	#joins = new Map<string, { budgets: Counts<number>; min: Duration | null; end: bigint | undefined }>();
	// The last union read, until a change moves it.
	#union?: Range[];

	/** Count `range`, returning whether the union changed. */
	add(range: Range): boolean {
		const shape = shapeOf(range);
		let changed = false;
		this.held++;
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
			case "oldest": {
				const furthest = this.#oldest.last();
				this.#oldest.add(shape.end);
				changed = (furthest === undefined || shape.end > furthest) && this.#open === 0;
				break;
			}
		}
		return this.#moved(changed);
	}

	/** Uncount `range`, one {@link add} counted, returning whether the union changed. */
	remove(range: Range): boolean {
		const shape = shapeOf(range);
		let changed = false;
		this.held--;
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
			case "oldest": {
				const furthest = this.#oldest.last();
				this.#oldest.remove(shape.end);
				changed = this.#oldest.last() !== furthest && this.#open === 0;
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

	/**
	 * The union at the default policy in a canonical order, costing its own length rather than the
	 * audience's. Read only.
	 */
	union(): readonly Range[] {
		this.#union ??= this.#fold();
		return this.#union;
	}

	#fold(): Range[] {
		if (this.#open > 0) return [{}];
		const union: Range[] = this.#time
			.runs()
			.map((run) => (run.end === UNBOUNDED ? { start: run.start } : { start: run.start, end: run.end - 1n }));
		const furthest = this.#oldest.last();
		if (furthest !== undefined) union.push({ end: furthest });
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
