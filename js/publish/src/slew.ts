// The media clock's arithmetic, driven by the caller's reads, so tests step it without a clock.

// The most the timeline gains or loses on the monotonic clock, as a divisor: 500 ppm.
const SLEW = 2_000;
// `Date.now()` reads whole milliseconds, so an error within two of them is noise, left alone.
const SETTLED = 2_000;
// A forward wall jump past this between two reads, in microseconds, is a suspend, not drift.
const GAP = 2_000_000;

/**
 * A timeline that advances with the monotonic clock, nudged toward the wall clock by at most 500 ppm,
 * and jumping forward across a suspend. All values are microseconds from instant zero.
 */
export class Slew {
	// The instant of the last read, how far the wall clock read past it then, and the reading.
	#last?: { at: number; offset: number; reading: number };
	#jumped = 0;

	/** How far the timeline has jumped across suspends, in microseconds. */
	get jumped(): number {
		return this.#jumped;
	}

	/**
	 * What the timeline reads at instant `at`, where the wall clock puts it at `target`.
	 *
	 * The first read starts on the wall clock, since nothing is stamped yet; so does one before the
	 * last, which only another clock installed since can give.
	 */
	read(at: number, target: number): number {
		const offset = target - at;
		const last = this.#last;
		if (last === undefined || at < last.at) {
			this.#last = { at, offset, reading: Math.max(target, 0) };
			return this.#last.reading;
		}

		const elapsed = at - last.at;
		let reading = last.reading + elapsed;
		const jump = offset - last.offset;
		// A suspend: the timeline jumps the gap, keeping whatever error it had.
		if (jump > GAP) {
			reading += jump;
			this.#jumped += jump;
		}
		const error = target - reading;
		if (Math.abs(error) > SETTLED) {
			const cap = elapsed / SLEW;
			reading += Math.min(Math.max(error, -cap), cap);
		}
		this.#last = { at, offset, reading };
		return reading;
	}
}
