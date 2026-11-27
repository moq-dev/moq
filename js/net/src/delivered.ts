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
	 * Bridge a declared skip `[start, end)` into `delivered` only when it strictly connects two
	 * already-handed-out runs (`start - 1` and `end` are both in `delivered`). Strictly reduces
	 * the run count without extending `delivered` below the lowest or above the highest
	 * handed-out group. Mirrors the Rust `Delivered::bridge_skip`.
	 */
	bridgeSkip(start: bigint, end: bigint): boolean {
		if (start >= end || start === 0n) return false;
		if (this.has(start - 1n) && this.has(end)) {
			this.addRun(start, end);
			return true;
		}
		return false;
	}

	/**
	 * Bridge the immediate declared-skip gaps adjacent to `key`'s run that satisfy `allow`,
	 * invoking `onBridge` for each bridged gap. Only connects already-handed-out runs; never
	 * extends `delivered` below the lowest or above the highest handed-out group. Mirrors the
	 * Rust `Delivered::bridge_adjacent_skips`.
	 */
	bridgeAdjacentSkips(
		key: bigint,
		skipEnd: (from: bigint) => bigint | undefined,
		allow: (start: bigint, end: bigint) => boolean,
		onBridge?: (start: bigint, end: bigint) => void,
	): void {
		if (this.#runs.length < 2) return;
		const at = this.#below(key);
		if (at > 0) {
			const curStart = this.#runs[at][0];
			const prevEnd = this.#runs[at - 1][1] + 1n;
			const end = skipEnd(prevEnd);
			if (prevEnd < curStart && end !== undefined && end >= curStart && allow(prevEnd, curStart)) {
				this.addRun(prevEnd, curStart);
				onBridge?.(prevEnd, curStart);
			}
		}
		const after = this.runEnd(key);
		if (after !== undefined) {
			const nextIdx = this.#below(after) + 1;
			if (nextIdx < this.#runs.length) {
				const nextStart = this.#runs[nextIdx][0];
				const end = skipEnd(after);
				if (nextStart > after && end !== undefined && end >= nextStart && allow(after, nextStart)) {
					this.addRun(after, nextStart);
					onBridge?.(after, nextStart);
				}
			}
		}
	}

	/**
	 * Bridge every gap inside declared skip `[skipStart, skipEnd)` that sits strictly between
	 * two already-handed-out runs of `this` and satisfies `allow`, invoking `onBridge` for each
	 * bridged gap. Never extends `delivered` below the lowest or above the highest handed-out
	 * group. Mirrors the Rust `Delivered::bridge_skip_gaps`.
	 */
	bridgeSkipGaps(
		skipStart: bigint,
		skipEnd: bigint,
		allow: (start: bigint, end: bigint) => boolean,
		onBridge?: (start: bigint, end: bigint) => void,
	): void {
		if (skipStart >= skipEnd || this.#runs.length < 2) return;
		let gapStart: bigint | undefined;
		const below = this.#below(skipStart);
		if (below >= 0 && this.#runs[below][1] + 1n >= skipStart) {
			gapStart = this.#runs[below][1] + 1n;
		} else if (below + 1 < this.#runs.length && this.#runs[below + 1][0] < skipEnd) {
			gapStart = this.#runs[below + 1][1] + 1n;
		}
		while (gapStart !== undefined && gapStart < skipEnd) {
			const nextIdx = this.#below(gapStart) + 1;
			if (nextIdx >= this.#runs.length) break;
			const nextStart = this.#runs[nextIdx][0];
			if (nextStart > skipEnd) break;
			if (allow(gapStart, nextStart)) {
				this.addRun(gapStart, nextStart);
				onBridge?.(gapStart, nextStart);
			}
			gapStart = this.runEnd(nextStart);
		}
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
