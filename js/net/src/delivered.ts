/**
 * The group keys a reader handed out, so it never hands one out twice. Mirrors
 * `model::delivered` in `rs/moq-net`.
 *
 * @module
 */

import { max, min } from "./util/key.ts";
import type { Run } from "./window.ts";

/** The groups a reader's ranges hold, as a fold judges a gap by. */
export interface Judged {
	/** The runs the placed ranges name from `from` up to the exclusive `end`, lowest first. */
	placed(from: bigint, end: bigint): Run[];
}

/**
 * The group keys a reader handed out, as runs of consecutive keys.
 *
 * A reader stops expecting a group once it is past its budget, and counts everything below it
 * as handed out (see {@link forgetBelow}). The run starting at 0 is that floor, so only the
 * gaps above it cost memory.
 */
export class Delivered {
	// First and last key of each run, inclusive, sorted and never adjacent to each other.
	#runs: [bigint, bigint][] = [];
	// The keys a fold counted as handed out that no range held then, so no budget judged them: a
	// range added later owes each it holds (see {@link reopen}). Always within `#runs`, and never
	// more runs than {@link foldBelow} leaves. Mirrors the Rust `Delivered::unjudged`.
	#unjudged: Delivered | undefined;

	// The index of the run containing or preceding `key`, or -1 when none starts at or below it.
	#below(key: bigint): number {
		let lo = 0;
		let hi = this.#runs.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#runs[mid][0] <= key) lo = mid + 1;
			else hi = mid;
		}
		return lo - 1;
	}

	/** Whether `key` was handed out, or is below the floor. */
	has(key: bigint): boolean {
		const index = this.#below(key);
		return index >= 0 && key <= this.#runs[index][1];
	}

	/** Whether every key in `[start, end)` was handed out, or is below the floor. */
	covers(start: bigint, end: bigint): boolean {
		if (start >= end) return true;
		const index = this.#below(start);
		return index >= 0 && end - 1n <= this.#runs[index][1];
	}

	/** Record `key` as handed out; `false` when it already was. */
	add(key: bigint): boolean {
		if (this.has(key)) return false;
		this.addRun(key, key + 1n);
		return true;
	}

	/** Record every key in `[start, end)` as handed out. */
	addRun(start: bigint, end: bigint): void {
		if (start >= end) return;
		this.#unjudged?.remove(start, end);
		// The runs it overlaps or touches, `first` through `last`, merge into one.
		let first = this.#below(start - 1n);
		if (first < 0 || this.#runs[first][1] < start - 1n) first++;
		const last = this.#below(end);
		if (last < first) {
			this.#runs.splice(first, 0, [start, end - 1n]);
			return;
		}
		const merged: [bigint, bigint] = [min(start, this.#runs[first][0]), max(end - 1n, this.#runs[last][1])];
		this.#runs.splice(first, last - first + 1, merged);
	}

	/** Forget that any key in `[start, end)` was handed out, splitting the runs it cuts. */
	remove(start: bigint, end: bigint): void {
		if (start >= end) return;
		this.#unjudged?.remove(start, end);
		// The runs reaching into it, `first` through `last`, keep only what lies outside it.
		let first = this.#below(start);
		if (first < 0 || this.#runs[first][1] < start) first++;
		const last = this.#below(end - 1n);
		if (last < first) return;
		const kept: [bigint, bigint][] = [];
		if (this.#runs[first][0] < start) kept.push([this.#runs[first][0], start - 1n]);
		if (this.#runs[last][1] >= end) kept.push([end, this.#runs[last][1]]);
		this.#runs.splice(first, last - first + 1, ...kept);
	}

	/** A record of what either this or `other` handed out. */
	union(other: Delivered): Delivered {
		const union = new Delivered();
		union.#runs = this.#runs.map(([first, last]) => [first, last]);
		for (const [first, last] of other.#runs) union.addRun(first, last + 1n);
		return union;
	}

	/** The parts of `[start, end)` not handed out, in order. */
	*gaps(start: bigint, end: bigint): Generator<{ start: bigint; end: bigint }> {
		let next = start;
		for (let index = Math.max(this.#below(start), 0); index < this.#runs.length && next < end; index++) {
			const [first, last] = this.#runs[index];
			if (first > next) yield { start: next, end: min(first, end) };
			next = max(next, last + 1n);
		}
		if (next < end) yield { start: next, end };
	}

	/**
	 * Count every key below `floor` as handed out, as a fold past the reader's budget does: each gap
	 * it closes that `judged` doesn't hold, or every one while the ranges are still being placed, a
	 * range added later that holds it owes again. Mirrors the Rust `Delivered::fold_below`.
	 *
	 * So that ranges moving to disjoint groups leave no more runs than they hold, what lies between
	 * two runs it remembers that `judged` doesn't hold joins them: what ranges added over it later
	 * hand out may then include a group handed out before, which the same identity makes harmless,
	 * rather than miss one never handed out.
	 */
	foldBelow(floor: bigint, judged: Judged | undefined): void {
		const gaps = [...this.gaps(this.runEnd(0n) ?? 0n, floor)];
		for (const gap of gaps) {
			let from = gap.start;
			for (const held of judged?.placed(gap.start, gap.end) ?? []) {
				if (held.start > from) this.#unjudge(from, held.start);
				from = max(from, held.end);
			}
			if (from < gap.end) this.#unjudge(from, gap.end);
		}
		this.forgetBelow(floor);
		// Placed ranges say what joins; a fold while they're placed is followed by one that is.
		const unjudged = this.#unjudged;
		if (!judged || gaps.length === 0 || !unjudged || unjudged.#runs.length < 2) return;
		const [start, end] = [unjudged.#runs[0][0], unjudged.#runs[unjudged.#runs.length - 1][1] + 1n];
		let from = start;
		for (const held of [...judged.placed(start, end), { start: end, end }]) {
			if (held.start > from) {
				unjudged.addRun(from, held.start);
				// Not what a reopen gave back since, which is owed rather than counted handed out.
				for (const gap of [...this.gaps(from, held.start)]) unjudged.remove(gap.start, gap.end);
			}
			from = max(from, held.end);
		}
	}

	// Remember `[start, end)` as folded unjudged.
	#unjudge(start: bigint, end: bigint): void {
		this.#unjudged ??= new Delivered();
		this.#unjudged.addRun(start, end);
	}

	/** Take back each key a fold counted as handed out unjudged that `judged` now holds, so it is owed again. */
	reopen(judged: Judged): void {
		const unjudged = this.#unjudged;
		if (!unjudged) return;
		const held = unjudged.#runs.flatMap(([first, last]) => judged.placed(first, last + 1n));
		for (const run of held) this.remove(run.start, run.end);
	}

	/** Count every key below `floor` as handed out. */
	forgetBelow(floor: bigint): void {
		if (floor <= 0n) return;
		// Every run starting at or below the floor joins it, including one starting right at it.
		const index = this.#below(floor);
		const last = max(floor - 1n, index >= 0 ? this.#runs[index][1] : 0n);
		this.#runs.splice(0, index + 1, [0n, last]);
	}

	/**
	 * The lowest key handed out above a gap: once it is past the reader's budget, so is
	 * the gap, since group timestamps on a track never rewind.
	 */
	aboveGap(): bigint | undefined {
		const first = this.#runs[0];
		if (!first) return undefined;
		return first[0] === 0n ? this.#runs[1]?.[0] : first[0];
	}

	/** One past the last key of the run holding `key`, if it was handed out. */
	runEnd(key: bigint): bigint | undefined {
		const index = this.#below(key);
		return index >= 0 && key <= this.#runs[index][1] ? this.#runs[index][1] + 1n : undefined;
	}

	/** How many runs it holds: one per gap, plus the floor, plus one per run folded unjudged. */
	get runs(): number {
		return this.#runs.length + (this.#unjudged?.runs ?? 0);
	}
}
