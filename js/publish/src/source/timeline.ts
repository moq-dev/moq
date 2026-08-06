import { Time } from "@moq/net";

/**
 * Maps a file's presentation timeline onto our wall clock, repeating it forever.
 *
 * One instance is shared by a file's audio and video so both restart at the same instant. Without
 * that they would drift apart by the difference between the two tracks' durations on every loop.
 *
 * Not exported from `source/index.ts`: this is an implementation detail of the file source.
 */
export class Timeline {
	readonly #zero: Time.Micro;
	readonly #duration: Time.Micro;

	/**
	 * Anchor a file of `duration` seconds at wall clock time `zero`.
	 *
	 * A file with no usable duration (unknown, zero, or infinite) can't be looped, only played once.
	 */
	constructor(zero: Time.Micro, duration: number) {
		this.#zero = zero;
		this.#duration =
			Number.isFinite(duration) && duration > 0
				? Time.Micro.fromSecond(duration as Time.Second)
				: (0 as Time.Micro);
	}

	/** Whether the file repeats rather than ending after one pass. */
	get loops(): boolean {
		return this.#duration > 0;
	}

	/** When a file timestamp on the given pass should be published, on our wall clock. */
	at(timestamp: Time.Micro, loop: number): Time.Micro {
		return (this.#zero + timestamp + loop * this.#duration) as Time.Micro;
	}
}
