import type { Time } from "@moq/net";

// Implements the estimator in doc/concept/audio-jitter.md, which is normative. The design and
// every constant come from WebRTC NetEq's underrun optimizer; this is written from the document,
// not from NetEq's source.

/** Milliseconds of media time the reference search looks back over. */
const WINDOW = 2000;

/** Milliseconds of delay per histogram bucket. */
const BUCKET = 20;

/** Histogram buckets, so the measured term saturates at `BUCKET * BUCKETS`. */
const BUCKETS = 100;

/** The quantile of the delay distribution the estimate is read at. */
const QUANTILE = 0.95;

/** Weight retained per folded observation, a ~20s half-life at one fold per `RESAMPLE`. */
const FORGET = 0.983;

/** Milliseconds of arrival time per resampled observation. */
const RESAMPLE = 500;

/** Holds the forget factor below `FORGET` early on, so evidence overwrites the prior. */
const RAMP = 2;

/** Idle intervals folded at once, purely to bound the work. */
const IDLE_MAX = 1024;

/** The cold-start distribution, which is also what an idle interval relaxes back to. */
const PRIOR = Float64Array.from({ length: BUCKETS }, (_, i) => 0.5 ** (i + 1));

/**
 * How much buffer late arrivals need, measured from arrival timing alone.
 *
 * Each frame's delay is measured against the fastest frame in the last `WINDOW` of media time, the
 * largest delay per `RESAMPLE` interval lands in a decaying histogram, and the estimate is its 95th
 * percentile. Excludes the codec frame and any advertised floor, which the caller composes on top.
 */
export class Jitter {
	/** Arrivals still inside the reference window, in media order. */
	#history: { arrival: number; media: number }[] = [];
	/** The largest media timestamp observed, so a reordered frame is recognized. */
	#newest = Number.NEGATIVE_INFINITY;

	/** The arrival resample intervals are counted from, unset until the first observation. */
	#origin?: number;
	/** The interval currently accumulating, counted from `#origin`. */
	#interval = 0;
	/** The largest delay seen in that interval. */
	#peak = 0;

	#histogram = Float64Array.from(PRIOR);
	/** Observations folded in, which drives the cold-start ramp. */
	#count = 0;

	/** Fold one frame in: `arrival` on the receiver's monotonic clock, `media` its timestamp. */
	observe(arrival: Time.Milli, media: Time.Milli): void {
		// A reordered frame is excluded rather than measured: the media-time gap back to the frame
		// that overtook it would otherwise be added to the delay.
		if (media <= this.#newest) return;
		this.#newest = media;

		// Pruning by media time is what makes a timestamp jump self-correcting: everything before it
		// leaves the window at once and the new frame becomes its own reference.
		let stale = 0;
		while (stale < this.#history.length && this.#history[stale].media < media - WINDOW) stale++;
		if (stale > 0) this.#history.splice(0, stale);
		this.#history.push({ arrival, media });

		// Delay against the fastest frame in the window, not the previous one, so a queue that
		// fills a little per frame reads as the build-up it is.
		let reference = this.#history[0];
		for (const entry of this.#history) {
			if (entry.arrival - entry.media < reference.arrival - reference.media) reference = entry;
		}
		const delay = Math.max(0, arrival - reference.arrival - (media - reference.media));

		if (this.#origin === undefined) {
			this.#origin = arrival;
			this.#peak = delay;
			return;
		}

		const interval = Math.floor((arrival - this.#origin) / RESAMPLE);
		if (interval <= this.#interval) {
			this.#peak = Math.max(this.#peak, delay);
			return;
		}

		this.#fold(Math.min(BUCKETS - 1, Math.floor(this.#peak / BUCKET)));

		// An interval with no arrival relaxes the estimate toward the prior at the same rate.
		const idle = Math.min(interval - this.#interval - 1, IDLE_MAX);
		for (let i = 0; i < idle; i++) this.#fold(undefined);

		this.#interval = interval;
		this.#peak = delay;
	}

	/** The measured term: the upper edge of the bucket holding the 95th percentile. */
	get measured(): Time.Milli {
		let cumulative = 0;
		for (let i = 0; i < BUCKETS; i++) {
			cumulative += this.#histogram[i];
			if (cumulative > QUANTILE) return ((i + 1) * BUCKET) as Time.Milli;
		}
		return (BUCKETS * BUCKET) as Time.Milli;
	}

	// `h = f*h + (1-f)*x`, where x is all weight on the observed bucket, or the prior when the
	// interval held no arrival. The total stays at 1 without renormalizing.
	#fold(bucket: number | undefined): void {
		this.#count += 1;
		const forget = Math.min(FORGET, this.#count / (this.#count + RAMP));
		const fresh = 1 - forget;

		for (let i = 0; i < BUCKETS; i++) {
			this.#histogram[i] *= forget;
			if (bucket === undefined) this.#histogram[i] += fresh * PRIOR[i];
		}
		if (bucket !== undefined) this.#histogram[bucket] += fresh;
	}
}
