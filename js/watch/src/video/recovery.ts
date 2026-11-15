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
	// The pictures still warming up since the last join or gap: from its recovery point, for the
	// warmup. `passed` once the codec output a picture past it, so none of it can still come out.
	#warming?: { start: Duration; end: Duration; passed: boolean };

	constructor(warmup: Duration = Duration.ZERO) {
		this.#warmup = warmup;
	}

	/** Arm recovery after media was lost, preserving continuous groups and declared ends. */
	gap(): void {
		this.#needed = true;
	}

	/**
	 * A fresh encoder starts another timeline after a declared end. Its first picture references
	 * nothing earlier and needs no warmup, even after a join or a gap.
	 */
	restart(): void {
		this.#needed = false;
		this.#floor = undefined;
		this.#warming = undefined;
	}

	/** Whether this access unit has the references available at the current delivery boundary. */
	accept(frame: Container.ReceivedFrame): boolean {
		if (frame.keyframe) {
			if (this.#needed && !this.#warmup.isZero()) {
				// The codec may still hold pictures of a warmup it has not output past, which stay
				// hidden: the hidden span reaches back over them rather than growing a list.
				const previous = this.#warming;
				const start = previous && !previous.passed ? previous.start.min(frame.timestamp) : frame.timestamp;
				this.#warming = { start, end: frame.timestamp.add(this.#warmup), passed: false };
			}
			this.#floor = this.#needed ? frame.timestamp : undefined;
			this.#needed = false;
		}
		return this.#floor === undefined || frame.timestamp.ge(this.#floor);
	}

	/**
	 * Whether the decoded picture at `timestamp` is still warming up after the last join or gap,
	 * so it is decoded but not shown. Called on each picture the codec outputs, in presentation
	 * order.
	 */
	warming(timestamp: Duration): boolean {
		const warming = this.#warming;
		if (warming === undefined) return false;
		if (timestamp.ge(warming.end)) {
			warming.passed = true;
			return false;
		}
		return timestamp.ge(warming.start);
	}
}
