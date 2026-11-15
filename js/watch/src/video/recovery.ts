import { Duration } from "@moq/clock";
import type { Container } from "@moq/hang";

/**
 * Refuse leading pictures whose previous GOP was not delivered to this decoder, and hold back the
 * catalog `warmup` of pictures after each join, which a recovery point is not clean before. @internal
 */
export class Recovery {
	#needed = true;
	#floor?: Duration;
	readonly #warmup: Duration;
	// The pictures still warming up since the last join or gap: from its recovery point, for the warmup.
	#warming?: { start: Duration; end: Duration };

	constructor(warmup: Duration = Duration.ZERO) {
		this.#warmup = warmup;
	}

	/** Arm recovery after media was lost, preserving continuous groups and declared ends. */
	gap(): void {
		this.#needed = true;
	}

	/** Forget the warmup once a fresh encoder starts another timeline, whose pictures need none. */
	restart(): void {
		this.#warming = undefined;
	}

	/** Whether this access unit has the references available at the current delivery boundary. */
	accept(frame: Container.ReceivedFrame): boolean {
		if (frame.keyframe) {
			if (this.#needed && !this.#warmup.isZero()) {
				this.#warming = { start: frame.timestamp, end: frame.timestamp.add(this.#warmup) };
			}
			this.#floor = this.#needed ? frame.timestamp : undefined;
			this.#needed = false;
		}
		return this.#floor === undefined || frame.timestamp.ge(this.#floor);
	}

	/**
	 * Whether the picture at `timestamp` is still warming up after the last join or gap, so it is
	 * decoded but neither shown nor placed on the clock. Pictures from before that recovery point
	 * that the codec still holds are not.
	 */
	warming(timestamp: Duration): boolean {
		const warming = this.#warming;
		return warming !== undefined && timestamp.ge(warming.start) && timestamp.lt(warming.end);
	}
}
