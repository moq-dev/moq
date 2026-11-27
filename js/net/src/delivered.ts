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
	 * Bridge declared-skip gaps around `key` and at the oldest gap (`aboveGap`), invoking
	 * `onBridge` for each bridged gap. Only connects already-handed-out runs; never extends
	 * `delivered` below the lowest or above the highest handed-out group. Mirrors the Rust
	 * `Delivered::bridge_skips`.
	 */
	bridgeSkips(
		key: bigint,
		skipEnd: (from: bigint) => bigint | undefined,
		onBridge?: (start: bigint, end: bigint) => void,
	): void {
		// Walk backward from the run holding `key`, allowing one still-open gap right below it so
		// overlapping live groups still collapse even if a permanent hole sits lower down.
		const at = this.#below(key);
		let cur = at >= 0 ? this.#runs[at][0] : key;
		let allowedOpen = true;
		for (;;) {
			const prev = this.#below(cur - 1n);
			if (prev < 0) break;
			const [prevStart, prevLast] = this.#runs[prev];
			const prevEnd = prevLast + 1n;
			if (prevEnd >= cur) break;
			const end = skipEnd(prevEnd);
			if (end !== undefined && end >= cur) {
				this.addRun(prevEnd, cur);
				onBridge?.(prevEnd, cur);
				cur = prevStart;
			} else if (allowedOpen) {
				allowedOpen = false;
				cur = prevStart;
			} else {
				break;
			}
		}
		// Bridge the immediate gap above the run holding `key`, if `key` landed below an
		// already-handed-out group.
		const after = this.runEnd(key);
		if (after !== undefined) {
			const nextIdx = this.#below(after) + 1;
			if (nextIdx < this.#runs.length) {
				const nextStart = this.#runs[nextIdx][0];
				const end = skipEnd(after);
				if (nextStart > after && end !== undefined && end >= nextStart) {
					this.addRun(after, nextStart);
					onBridge?.(after, nextStart);
				}
			}
		}
		// Collapse any oldest gap that became skipped since the groups around it were handed out.
		for (;;) {
			const above = this.aboveGap();
			if (above === undefined) break;
			const prev = this.#below(above - 1n);
			if (prev < 0) break;
			const below = this.#runs[prev][1] + 1n;
			if (below >= above) break;
			const end = skipEnd(below);
			if (end !== undefined && end >= above) {
				this.addRun(below, above);
				onBridge?.(below, above);
			} else {
				break;
			}
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
