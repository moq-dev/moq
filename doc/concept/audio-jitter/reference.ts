// A reference implementation of the estimator described in /concept/audio-jitter,
// used only to generate the conformance corpus beside it.
//
// It is not normative and is not shipped in any package. The document is normative:
// when the two disagree the document wins, this file is corrected, and the corpus is
// regenerated. Nothing outside this directory may import it, because an
// implementation that calls the reference proves nothing about the document.

/** Milliseconds of media time the reference search looks back over. */
export const WINDOW = 2000;

/** Milliseconds of delay per histogram bucket. */
export const BUCKET = 20;

/** Histogram buckets, so the measured term saturates at BUCKET * BUCKETS. */
export const BUCKETS = 100;

/** The quantile of the delay distribution the target is read at. */
export const QUANTILE = 0.95;

/** Weight retained per folded observation. */
export const FORGET = 0.983;

/** Milliseconds of arrival time per resampled observation. */
export const RESAMPLE = 500;

/** Weight of the cold-start ramp, which holds the forget factor below FORGET early on. */
export const RAMP = 2;

/** Idle intervals folded at once, purely to bound the work. */
export const IDLE_MAX = 1024;

/** The cold-start distribution, which is also what an idle receiver relaxes back to. */
export function prior(): Float64Array {
	const out = new Float64Array(BUCKETS);
	for (let i = 0; i < BUCKETS; i++) {
		out[i] = 0.5 ** (i + 1);
	}
	return out;
}

/** One frame as the receiver saw it, in milliseconds. */
export interface Arrival {
	/** The receiver's monotonic clock when the frame came off the transport. */
	arrival: number;
	/** The frame's container timestamp, converted to milliseconds. */
	media: number;
}

/** What the receiver knows about the track before any frame arrives. */
export interface Config {
	/** The codec's frame duration, never learned from observed timestamps. */
	frame: number;
	/** The publisher's advertised flush span, a floor on the measured term. */
	advertised?: number;
}

/** Estimates the audio playout target from arrival timing alone. */
export class Estimator {
	readonly #frame: number;
	readonly #advertised: number;

	/** Arrivals still inside the reference window, in media order. */
	#history: Arrival[] = [];
	/** The largest media timestamp observed, so a reordered frame is recognized. */
	#newest = Number.NEGATIVE_INFINITY;

	/** The arrival that resample intervals are counted from. */
	#origin?: number;
	/** The interval currently accumulating, counted from #origin. */
	#interval = 0;
	/** The largest delay seen in that interval. */
	#peak = 0;

	#histogram = prior();
	/** Observations folded in, which drives the cold-start ramp. */
	#count = 0;

	constructor(config: Config) {
		this.#frame = config.frame;
		this.#advertised = config.advertised ?? 0;
	}

	/** Fold one frame's arrival into the estimate. */
	observe({ arrival, media }: Arrival): void {
		// A reordered arrival is excluded rather than measured: the media-time gap back
		// to the frame that overtook it would otherwise be added to the delay.
		if (media <= this.#newest) return;
		this.#newest = media;

		// Pruning by media time rather than by arrival is what makes a timestamp jump
		// self-correcting: everything before it leaves the window at once.
		while (this.#history.length > 0 && this.#history[0].media < media - WINDOW) {
			this.#history.shift();
		}
		this.#history.push({ arrival, media });

		// The fastest frame still in the window, so delay is measured against the
		// best-case path instead of against the previous frame.
		let reference = this.#history[0];
		for (const entry of this.#history) {
			if (entry.arrival - entry.media < reference.arrival - reference.media) {
				reference = entry;
			}
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

		const idle = Math.min(interval - this.#interval - 1, IDLE_MAX);
		for (let i = 0; i < idle; i++) {
			this.#fold(undefined);
		}

		this.#interval = interval;
		this.#peak = delay;
	}

	/** The target the receiver holds, in milliseconds. */
	get target(): number {
		let cumulative = 0;
		let measured = BUCKETS * BUCKET;
		for (let i = 0; i < BUCKETS; i++) {
			cumulative += this.#histogram[i];
			if (cumulative > QUANTILE) {
				measured = (i + 1) * BUCKET;
				break;
			}
		}

		return Math.max(measured, this.#advertised) + this.#frame;
	}

	// One observation is `h = f*h + (1-f)*x`, where x is the bucket that was observed,
	// or the prior when the interval held no arrival at all. An idle interval therefore
	// relaxes the estimate back toward cold start at the same time constant, rather than
	// freezing it or renormalizing the decay away.
	#fold(bucket: number | undefined): void {
		this.#count += 1;
		const forget = Math.min(FORGET, this.#count / (this.#count + RAMP));
		const fresh = 1 - forget;
		const seed = bucket === undefined ? prior() : undefined;

		for (let i = 0; i < BUCKETS; i++) {
			this.#histogram[i] *= forget;
			if (seed) this.#histogram[i] += fresh * seed[i];
		}
		if (bucket !== undefined) this.#histogram[bucket] += fresh;
	}
}
