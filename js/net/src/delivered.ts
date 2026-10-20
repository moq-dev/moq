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

	/** Record `sequence` as handed out; `false` when it already was. */
	add(sequence: number): boolean {
		const index = this.#below(sequence);
		const run = this.#runs[index];
		if (run && sequence <= run[1]) return false;
		const next = this.#runs[index + 1];
		const joinsBelow = run !== undefined && run[1] + 1 === sequence;
		const joinsAbove = next !== undefined && next[0] === sequence + 1;
		if (joinsBelow && joinsAbove) {
			run[1] = next[1];
			this.#runs.splice(index + 1, 1);
		} else if (joinsBelow) {
			run[1] = sequence;
		} else if (joinsAbove) {
			next[0] = sequence;
		} else {
			this.#runs.splice(index + 1, 0, [sequence, sequence]);
		}
		return true;
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
