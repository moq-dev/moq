/**
 * The group keys a reader handed out, so it never hands one out twice. Mirrors
 * `model::delivered` in `rs/moq-net`.
 *
 * @module
 */

import { max, min } from "./util/key.ts";

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

	/**
	 * Absorb a declared skip `[start, end)` only where it extends an existing run rightward (or
	 * starts at 0), returning the subrange absorbed. Keeps consecutive sparse groups one run
	 * without adding a new non-zero run start. Mirrors the Rust `Delivered::absorb_skip`.
	 */
	absorbSkip(start: bigint, end: bigint): { start: bigint; end: bigint } | undefined {
		if (start >= end) return undefined;
		if (start === 0n || this.has(start - 1n) || this.has(start)) {
			this.addRun(start, end);
			return { start, end };
		}
		const next = this.#below(start) + 1;
		if (next < this.#runs.length && this.#runs[next][0] < end) {
			const from = this.#runs[next][0];
			this.addRun(from, end);
			return { start: from, end };
		}
		return undefined;
	}

	/** Forget that any key in `[start, end)` was handed out, splitting any run it cuts. */
	remove(start: bigint, end: bigint): void {
		if (start >= end) return;
		const last = end - 1n;
		let first = this.#below(start);
		if (first < 0 || this.#runs[first][1] < start) first++;
		let stop = first;
		while (stop < this.#runs.length && this.#runs[stop][0] <= last) stop++;
		if (first >= stop) return;
		const pieces: [bigint, bigint][] = [];
		const lowest = this.#runs[first];
		if (lowest[0] < start) pieces.push([lowest[0], start - 1n]);
		const highest = this.#runs[stop - 1];
		if (highest[1] > last) pieces.push([last + 1n, highest[1]]);
		this.#runs.splice(first, stop - first, ...pieces);
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

	/** How many runs it holds: one per gap, plus the floor. */
	get runs(): number {
		return this.#runs.length;
	}
}
