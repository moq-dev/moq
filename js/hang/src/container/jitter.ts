import type { Milli } from "@moq/clock";

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

/** The most weight one observation takes, so a lone one holds the quantile for one half-life at most. */
const FRESH_MAX = 0.1;

/** Idle intervals folded at once, purely to bound the work. */
const IDLE_MAX = 1024;

/**
 * Arrivals kept in the reference window.
 *
 * The window is media time, and a publisher chooses the timestamp rate, so an uncapped
 * history can hold millions of entries from a stream that advances a microsecond per
 * frame. This bounds both memory and a reordered frame's scan, and stays far above the
 * 2 s window of any real track (a 1000 fps track holds 2000).
 */
const MAX_HISTORY = 8192;

/** The cold-start distribution, which is also what an idle interval relaxes back to. */
const PRIOR = Float64Array.from({ length: BUCKETS }, (_, i) => 0.5 ** (i + 1));

/**
 * How much buffer late arrivals need, measured from arrival timing alone.
 *
 * Each frame's delay is measured against the fastest frame at or before it in the last `WINDOW` of
 * media time, the largest delay per `RESAMPLE` interval lands in a decaying histogram, and the
 * estimate is its 95th percentile. Excludes the codec frame and any advertised floor, which the
 * caller composes on top.
 */
export class Jitter {
	/** Arrivals still inside the reference window, in media order. */
	#history: { seq: number; arrival: number; media: number }[] = [];
	/** The first live entry in `#history`. */
	#historyStart = 0;
	/** The prefix minimum of `arrival - media` over `#history`, front first. */
	#minima: { seq: number; key: number; arrival: number; media: number }[] = [];
	/** The first live entry in `#minima`. */
	#minimaStart = 0;
	/** The next entry's position in the arrival sequence. */
	#nextSeq = 0;
	/** The largest media timestamp observed, so a reordered frame is recognized. */
	#newest = Number.NEGATIVE_INFINITY;

	/** The arrival resample intervals are counted from, unset until the first observation. */
	#origin?: number;
	/** The interval currently accumulating, counted from `#origin`. */
	#interval = 0;
	/** The largest delay seen in that interval. */
	#peak = 0;

	#histogram = Float64Array.from(PRIOR);
	/** How much of the histogram is still the prior's rather than evidence. */
	#prior = 1;

	/** Fold one frame in: `arrival` on the receiver's monotonic clock, `media` its timestamp. */
	observe(arrival: Milli, media: Milli): void {
		const key = arrival - media;

		// A reordered frame is measured but never joins the window: it arrived after a newer frame,
		// so it can never be the fastest, and a later frame must not be measured against it.
		if (media > this.#newest) {
			this.#newest = media;

			// Pruning by media time is what makes a timestamp jump self-correcting: everything before
			// it leaves the window at once and the new frame becomes its own reference.
			while (
				this.#historyStart < this.#history.length &&
				this.#history[this.#historyStart].media < media - WINDOW
			) {
				this.#historyStart++;
			}
			while (this.#history.length - this.#historyStart >= MAX_HISTORY) this.#historyStart++;

			const seq = this.#nextSeq++;
			this.#history.push({ seq, arrival, media });
			while (this.#minima.length > this.#minimaStart && this.#minima[this.#minima.length - 1].key >= key) {
				this.#minima.pop();
			}
			this.#minima.push({ seq, key, arrival, media });
			this.#trimMinima();
			this.#compact();
		}

		// Delay against the fastest frame in the window at or before this one, not the previous one,
		// so a queue that fills a little per frame reads as the build-up it is, and an older frame
		// held behind newer ones reads as the wait playing it in order costs. A frame older than the
		// whole window, such as the backlog a subscription delivers after its live edge, has no
		// reference and is not measured.
		const reference = this.#reference(media);
		if (reference === undefined) return;
		// Written in this order on purpose: the document requires bit-identical arithmetic,
		// so the difference is not reassociated.
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

	/** The fastest arrival at or before `media`, as its `(arrival, media)`. */
	#reference(media: number): { arrival: number; media: number } | undefined {
		if (media >= this.#newest) {
			// Every entry is at or before a frame in media order, and the deque's front is their minimum.
			return this.#minimaStart < this.#minima.length ? this.#minima[this.#minimaStart] : undefined;
		}

		// A reordered frame sees only a prefix, which the minimum deque cannot answer.
		// The history cap bounds the scan.
		let reference: { arrival: number; media: number } | undefined;
		for (let i = this.#historyStart; i < this.#history.length; i++) {
			const entry = this.#history[i];
			if (entry.media > media) break;
			if (!reference || entry.arrival - entry.media < reference.arrival - reference.media) reference = entry;
		}
		return reference;
	}

	/** Drop minimums whose arrivals left the history. */
	#trimMinima(): void {
		const front = this.#historyStart < this.#history.length ? this.#history[this.#historyStart].seq : undefined;
		if (front === undefined) {
			this.#minima.length = 0;
			this.#minimaStart = 0;
			return;
		}
		while (this.#minimaStart < this.#minima.length && this.#minima[this.#minimaStart].seq < front)
			this.#minimaStart++;
	}

	/** Reclaim the dropped prefix once it dominates, so memory does not grow with the stream. */
	#compact(): void {
		if (this.#historyStart > 1024 && this.#historyStart * 2 >= this.#history.length) {
			this.#history = this.#history.slice(this.#historyStart);
			this.#historyStart = 0;
		}
		if (this.#minimaStart > 1024 && this.#minimaStart * 2 >= this.#minima.length) {
			this.#minima = this.#minima.slice(this.#minimaStart);
			this.#minimaStart = 0;
		}
	}

	/**
	 * A new estimator that keeps what this one learned but measures arrivals afresh, for a track that
	 * replaces this one on the same path. Its frames are measured against their own fastest, never
	 * against this track's.
	 */
	fork(): Jitter {
		const fork = new Jitter();
		fork.#histogram = Float64Array.from(this.#histogram);
		fork.#prior = this.#prior;
		return fork;
	}

	/** @internal The history entries currently retained, for the cap's regression test. */
	get retained(): number {
		return this.#history.length - this.#historyStart;
	}

	/** The measured term: the upper edge of the bucket holding the 95th percentile. */
	get measured(): Milli {
		let cumulative = 0;
		for (let i = 0; i < BUCKETS; i++) {
			cumulative += this.#histogram[i];
			if (cumulative > QUANTILE) return ((i + 1) * BUCKET) as Milli;
		}
		return (BUCKETS * BUCKET) as Milli;
	}

	// Evidence decays by `FORGET` and the prior holds the rest, of which an observation takes up to
	// `FRESH_MAX`, so the prior is replaced within seconds while no single interval outweighs the
	// quantile's tail for long. Once the prior is gone this is `h = f*h + (1-f)*x`. An interval with
	// no arrival takes nothing, so its share returns to the prior. The total stays at 1 without
	// renormalizing.
	#fold(bucket: number | undefined): void {
		const rest = 1 - FORGET * (1 - this.#prior);
		const fresh = bucket === undefined ? 0 : Math.min(FRESH_MAX, rest);
		const left = rest - fresh;
		const seed = left - FORGET * this.#prior;

		for (let i = 0; i < BUCKETS; i++) {
			this.#histogram[i] = FORGET * this.#histogram[i] + seed * PRIOR[i];
		}
		if (bucket !== undefined) this.#histogram[bucket] += fresh;
		this.#prior = left;
	}
}
