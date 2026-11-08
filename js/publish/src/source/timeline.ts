import { Duration, Instant } from "@moq/clock";

/**
 * Maps a file's presentation timeline onto our clock, repeating it forever.
 *
 * One instance is shared by a file's audio and video so both restart at the same instant. Without
 * that they would drift apart by the difference between the two tracks' durations on every loop.
 *
 * Not exported from `source/index.ts`: this is an implementation detail of the file source.
 */
export class Timeline {
	readonly #zero: Instant;
	readonly #duration: Duration;

	/**
	 * Anchor a file of `duration` seconds at clock time `zero`.
	 *
	 * A file with no usable duration (unknown, zero, or infinite) can't be looped, only played once.
	 */
	constructor(zero: Instant, duration: number) {
		this.#zero = zero;
		this.#duration = Number.isFinite(duration) && duration > 0 ? Duration.secs(duration) : Duration.ZERO;
	}

	/** Whether the file repeats rather than ending after one pass. */
	get loops(): boolean {
		return !this.#duration.isZero();
	}

	/**
	 * When a file timestamp, in signed microseconds, on the given pass should be published, on our
	 * clock. A source timestamp can be negative, such as audio priming an edit list shifts before
	 * zero, so it stays signed until it lands on the clock, which saturates at the clock's origin.
	 */
	at(micros: number, loop: number): Instant {
		const start = this.#zero.add(this.#duration.mul(loop));
		if (micros >= 0) return start.add(Duration.micros(micros));
		return Instant.ZERO.add(start.since(Instant.ZERO).saturatingSub(Duration.micros(-micros)));
	}

	/** The file timestamp and pass due at the given clock time, the inverse of {@link at}. */
	position(at: Instant): { timestamp: Duration; loop: number } {
		const elapsed = at.since(this.#zero);
		const loop = this.loops ? Math.floor(elapsed.divDuration(this.#duration)) : 0;
		return { timestamp: elapsed.sub(this.#duration.mul(loop)), loop };
	}
}
