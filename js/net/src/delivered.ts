/**
 * The group sequences a reader handed out, so it never hands one out twice. Mirrors
 * `model::delivered` in `rs/moq-net`.
 *
 * @module
 */

/**
 * The group sequences a reader handed out, as runs of consecutive sequences.
 *
 * A reader stops expecting a group once it is past its budget, and counts everything below it
 * as handed out (see {@link forgetBelow}). The run starting at 0 is that floor, so only the
 * gaps above it cost memory.
 */
export class Delivered {
	// First and last sequence of each run, inclusive, sorted and never adjacent to each other.
	#runs: [number, number][] = [];

	// The index of the run containing or preceding `sequence`, or -1 when none starts at or below it.
	#below(sequence: number): number {
		let lo = 0;
		let hi = this.#runs.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#runs[mid][0] <= sequence) lo = mid + 1;
			else hi = mid;
		}
		return lo - 1;
	}

	/** Whether `sequence` was handed out, or is below the floor. */
	has(sequence: number): boolean {
		const index = this.#below(sequence);
		return index >= 0 && sequence <= this.#runs[index][1];
	}

	/** Whether every sequence in `[start, end)` was handed out, or is below the floor. */
	covers(start: number, end: number): boolean {
		if (start >= end) return true;
		const index = this.#below(start);
		return index >= 0 && end - 1 <= this.#runs[index][1];
	}

	/** Record `sequence` as handed out; `false` when it already was. */
	add(sequence: number): boolean {
		if (this.has(sequence)) return false;
		this.addRun(sequence, sequence + 1);
		return true;
	}

	/** Record every sequence in `[start, end)` as handed out. */
	addRun(start: number, end: number): void {
		if (start >= end) return;
		// The runs it overlaps or touches, `first` through `last`, merge into one.
		let first = this.#below(start - 1);
		if (first < 0 || this.#runs[first][1] < start - 1) first++;
		const last = this.#below(end);
		if (last < first) {
			this.#runs.splice(first, 0, [start, end - 1]);
			return;
		}
		const merged: [number, number] = [
			Math.min(start, this.#runs[first][0]),
			Math.max(end - 1, this.#runs[last][1]),
		];
		this.#runs.splice(first, last - first + 1, merged);
	}

	/** The parts of `[start, end)` not handed out, in order. */
	gaps(start: number, end: number): { start: number; end: number }[] {
		const gaps: { start: number; end: number }[] = [];
		let next = start;
		for (let index = Math.max(this.#below(start), 0); index < this.#runs.length && next < end; index++) {
			const [first, last] = this.#runs[index];
			if (first > next) gaps.push({ start: next, end: Math.min(first, end) });
			next = Math.max(next, last + 1);
		}
		if (next < end) gaps.push({ start: next, end });
		return gaps;
	}

	/** Count every sequence below `floor` as handed out. */
	forgetBelow(floor: number): void {
		if (floor <= 0) return;
		// Every run starting at or below the floor joins it, including one starting right at it.
		const index = this.#below(floor);
		const last = Math.max(floor - 1, index >= 0 ? this.#runs[index][1] : 0);
		this.#runs.splice(0, index + 1, [0, last]);
	}

	/**
	 * The lowest sequence handed out above a gap: once it is past the reader's budget, so is
	 * the gap, since group timestamps on a track never rewind.
	 */
	aboveGap(): number | undefined {
		const first = this.#runs[0];
		if (!first) return undefined;
		return first[0] === 0 ? this.#runs[1]?.[0] : first[0];
	}

	/** How many runs it holds: one per gap, plus the floor. */
	get runs(): number {
		return this.#runs.length;
	}
}
