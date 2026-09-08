import { Time } from "@moq/net";
import { type Getter, Signal } from "@moq/signals";

/** Width of one histogram bucket, which is also the resolution of the estimate. */
const BUCKET = 5;

/** Number of buckets, so the estimate saturates at `BUCKET * BUCKETS`. */
const BUCKETS = 200;

/**
 * How much an arrival still counts once another arrives. 0.9993 halves a sample's weight after
 * ~1000 arrivals, roughly 20s of 50/s audio: long enough to hold a rare burst, short enough to
 * forget a network that has since settled.
 */
const FORGET = 0.9993;

/** The fraction of arrivals the estimate covers. */
const PERCENTILE = 0.95;

/** How long the estimate holds its value before it may step down again. */
const LOWER_INTERVAL = 1000;

/**
 * How long a minimum stays authoritative. The minimum is tracked over two windows and read as the
 * smaller of the two, so it survives at least this long and at most twice it. Without an expiry a
 * single early arrival would anchor the spread for the life of the stream, and a sender clock
 * drifting away from the receiver's would inflate it without bound.
 */
const WINDOW = 30_000;

/**
 * How much buffer late arrivals need, measured at arrival rather than derived from the round trip.
 *
 * Each arrival contributes `now - timestamp` relative to the running minimum of that difference,
 * which is the delay a player has to absorb to render the frame on time: zero for an evenly paced
 * sender, one flush span for a sender that emits a burst of frames at once. Samples land in a
 * histogram that forgets old arrivals, and the estimate is a high percentile of it plus one frame,
 * the shape WebRTC's NetEq uses. The extra frame is what keeps audio in the buffer at the bottom of
 * the swing the spread describes, rather than landing it on exactly zero.
 *
 * The estimate rises as soon as a late frame proves the buffer is too shallow, and lowers by at
 * most one frame per interval, so a refinement shrinks a viewer's buffer in steps it can absorb.
 */
export class Jitter {
	// Weighted count of arrivals per spread bucket, and their sum.
	#buckets = new Float64Array(BUCKETS);
	#total = 0;

	// Smallest arrival delay in the current window and in the one before it. See WINDOW.
	#current?: number;
	#previous?: number;
	#windowStart?: number;

	// Largest spread ever measured. Buckets are read at their upper edge, which covers the whole
	// spread the bucket might hold; this caps that at a spread actually seen, so an evenly paced
	// sender reads as zero rather than as one bucket.
	#max = 0;

	// Smallest positive gap between consecutive timestamps, i.e. the frame duration. It bounds how
	// fast the estimate steps down.
	#spacing?: number;
	#latest?: Time.Micro;

	// When the estimate last moved, so a step down waits out LOWER_INTERVAL.
	#lowered?: number;

	#value = new Signal<Time.Milli>(Time.Milli.zero);

	/** The current estimate: enough buffer to render `PERCENTILE` of arrivals on time. */
	readonly value: Getter<Time.Milli> = this.#value;

	/** Fold one frame into the estimate, given its media timestamp and the wall time it arrived. */
	observe(timestamp: Time.Micro, now: Time.Milli): void {
		const delay = now - Time.Milli.fromMicro(timestamp);

		// Roll the minimum window forward so a stale baseline expires.
		this.#windowStart ??= now;
		if (now - this.#windowStart >= WINDOW) {
			this.#previous = this.#current;
			this.#current = undefined;
			this.#windowStart = now;
		}
		this.#current = this.#current === undefined ? delay : Math.min(this.#current, delay);
		const min = this.#previous === undefined ? this.#current : Math.min(this.#current, this.#previous);

		// Learn the frame duration from the timeline itself; it is the step size for lowering.
		if (this.#latest !== undefined && timestamp > this.#latest) {
			const gap = Time.Milli.fromMicro((timestamp - this.#latest) as Time.Micro);
			this.#spacing = this.#spacing === undefined ? gap : Math.min(this.#spacing, gap);
		}
		if (this.#latest === undefined || timestamp > this.#latest) this.#latest = timestamp;

		const spread = Math.max(0, delay - min);
		this.#max = Math.max(this.#max, spread);
		const bucket = Math.min(BUCKETS - 1, Math.floor(spread / BUCKET));

		// Decay everything, then count this arrival, so a sample's weight is relative to how many
		// have arrived since rather than to how long ago it was.
		for (let i = 0; i < BUCKETS; i++) this.#buckets[i] *= FORGET;
		this.#total = this.#total * FORGET + 1;
		this.#buckets[bucket] += 1;

		this.#publish(now);
	}

	/**
	 * Forget the arrival baseline, keeping the measured spread.
	 *
	 * A discontinuity moves the media timeline underneath the measurement, so `now - timestamp`
	 * jumps by the size of the jump and the old minimum describes a timeline that no longer
	 * exists. The histogram is in spread space, which the jump does not move, so it survives.
	 */
	reanchor(): void {
		this.#current = undefined;
		this.#previous = undefined;
		this.#windowStart = undefined;
		this.#latest = undefined;
	}

	#publish(now: Time.Milli): void {
		// One frame on top of the percentile: the buffer swings by the spread the percentile
		// measures, so without it the bottom of that swing lands on an empty ring.
		const step = this.#spacing ?? BUCKET;
		const measured = this.#percentile() + step;
		const current = this.#value.peek();

		if (measured >= current) {
			this.#lowered = now;
			if (measured > current) this.#value.set(Time.Milli(measured));
			return;
		}

		// Lower gradually: the buffer shrinks by at most one frame per interval, so a refined
		// estimate never drops the playhead onto an empty ring.
		this.#lowered ??= now;
		if (now - this.#lowered < LOWER_INTERVAL) return;
		this.#lowered = now;

		this.#value.set(Time.Milli(Math.max(measured, current - step)));
	}

	#percentile(): Time.Milli {
		if (this.#total <= 0) return Time.Milli.zero;

		const want = this.#total * PERCENTILE;
		let sum = 0;
		for (let i = 0; i < BUCKETS; i++) {
			sum += this.#buckets[i];
			// The upper edge of the bucket: the spread it holds is somewhere inside it, so covering
			// the whole bucket is what actually covers the percentile.
			if (sum >= want) return Time.Milli(Math.min((i + 1) * BUCKET, this.#max));
		}

		return Time.Milli(Math.min(BUCKETS * BUCKET, this.#max));
	}
}
