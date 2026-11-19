import { Duration, Instant } from "@moq/clock";
import * as MediaClock from "../media-clock";
import { jumped } from "../media-timeline";

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
	// How far the file is placed past the installed clock on the media clock, in microseconds.
	#shift: number;
	// The media clock's suspend jumps, in microseconds, as of the last stamp.
	#jumped: number;

	/**
	 * Anchor a file of `duration` seconds at clock time `zero`.
	 *
	 * A file with no usable duration (unknown, zero, or infinite) can't be looped, only played once.
	 */
	constructor(zero: Instant, duration: number) {
		this.#zero = zero;
		this.#shift = MediaClock.capture(zero).asMicros() - zero.since(Instant.ZERO).asMicros();
		this.#jumped = jumped();
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
		const start = this.#zero.saturatingAdd(this.#duration.mul(loop));
		if (micros >= 0) return start.saturatingAdd(Duration.micros(micros));
		return start.saturatingSub(Duration.micros(-micros));
	}

	/**
	 * The media clock timestamp of a clock time from {@link at}. A file is placed on the media clock
	 * where it starts and keeps its own spacing after that, so its audio stays contiguous, until the
	 * media clock jumps a suspend ahead of it: the file then jumps with it, rather than publish late.
	 */
	stamp(at: Instant): Duration {
		// Read first, so a jump since the last stamp is counted.
		MediaClock.now();
		const gap = jumped() - this.#jumped;
		// A new timeline (another clock installed) starts its count over; only a rise is a jump.
		if (gap > 0) this.#shift += gap;
		this.#jumped = jumped();
		return Duration.micros(Math.max(at.since(Instant.ZERO).asMicros() + this.#shift, 0));
	}

	/** The file timestamp and pass due at the given clock time, the inverse of {@link at}. */
	position(at: Instant): { timestamp: Duration; loop: number } {
		const elapsed = at.since(this.#zero);
		let loop = this.loops ? Math.floor(elapsed.divDuration(this.#duration)) : 0;
		// The quotient is a float, so step back a pass if it rounded up past the elapsed time.
		if (loop > 0 && this.#duration.mul(loop).gt(elapsed)) loop -= 1;
		return { timestamp: elapsed.sub(this.#duration.mul(loop)), loop };
	}
}
