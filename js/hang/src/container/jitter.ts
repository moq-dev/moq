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
 * Arrivals kept in the reference window, a power of two so it is also the ring's largest capacity.
 *
 * The window is media time, and a publisher chooses the timestamp rate, so an uncapped history
 * holds every frame of a stream that advances a nanosecond per frame. Lookups are logarithmic
 * either way; this bounds the memory, and stays far above the 2 s window of any real track (a
 * 1000 fps track holds 2000).
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
	#history = new History();
	/** The largest media timestamp observed, so a reordered frame is recognized. */
	#newest = Number.NEGATIVE_INFINITY;

	/** The arrival resample intervals are counted from, unset until the first observation. */
	#origin?: number;
	/** The interval currently accumulating, counted from `#origin`. */
	#interval = 0;
	/** The largest delay seen in that interval. */
	#peak = 0;
	/** A declared end precedes the next measured observation, so the silence before it is not idle. */
	#ended = false;
	/** The earliest a frame stalled in this interval was due. */
	#due = Number.POSITIVE_INFINITY;
	/** The latest a frame stalled in this interval arrived. */
	#late = Number.NEGATIVE_INFINITY;

	/** The bucket of the open stall's largest delay, folded once the stall ends. */
	#stall?: number;
	/** When the open stall's latest stalled frame arrived. */
	#stallLate = 0;
	/** Intervals the open stall spanned besides the one it folds as. */
	#span = 0;

	#histogram = Float64Array.from(PRIOR);
	/** How much of the histogram is still the prior's rather than evidence. */
	#prior = 1;

	/** Fold one frame in, in milliseconds: `arrival` on the receiver's monotonic clock, `media` its timestamp. */
	observe(arrival: number, media: number): void {
		// A reordered frame is measured but never joins the window: it arrived after a newer frame,
		// so it can never be the fastest, and a later frame must not be measured against it.
		if (media > this.#newest) {
			this.#newest = media;
			this.#history.push(arrival, media);
		}

		// Delay against the fastest frame in the window at or before this one, not the previous one,
		// so a queue that fills a little per frame reads as the build-up it is, and an older frame
		// held behind newer ones reads as the wait playing it in order costs. A frame older than the
		// whole window, such as the backlog a subscription delivers after its live edge, has no
		// reference and is not measured.
		const reference = this.#history.reference(media);
		if (reference === undefined) return;
		// Written in this order on purpose: the document requires bit-identical arithmetic,
		// so the difference is not reassociated.
		const delay = Math.max(0, arrival - reference.arrival - (media - reference.media));

		if (this.#origin === undefined) {
			this.#origin = arrival;
			this.#peak = delay;
			this.#note(arrival, delay);
			return;
		}

		const interval = Math.floor((arrival - this.#origin) / RESAMPLE);
		const resumed = this.#ended;
		this.#ended = false;
		if (interval <= this.#interval) {
			this.#peak = Math.max(this.#peak, delay);
			this.#note(arrival, delay);
			return;
		}

		this.#close(Math.min(BUCKETS - 1, Math.floor(this.#peak / BUCKET)));

		if (resumed) {
			// A silence the publisher declared says nothing about the path, so the estimate keeps
			// what it had and a fresh interval starts here.
			this.#settle();
			this.#origin = arrival;
			this.#interval = 0;
		} else {
			const idle = interval - this.#interval - 1;
			if (this.#stall !== undefined) {
				// Whether these belong to the stall depends on whether it continues after them.
				this.#span += idle;
			} else {
				// An interval with no arrival relaxes the estimate toward the prior at the same rate.
				for (let i = 0; i < Math.min(idle, IDLE_MAX); i++) this.#fold(undefined);
			}
			this.#interval = interval;
		}
		this.#peak = delay;
		this.#due = Number.POSITIVE_INFINITY;
		this.#late = Number.NEGATIVE_INFINITY;
		this.#note(arrival, delay);
	}

	// A frame later than a whole interval was due in an earlier one, so it is stalled.
	#note(arrival: number, delay: number): void {
		if (delay > RESAMPLE) {
			this.#due = Math.min(this.#due, arrival - delay);
			this.#late = Math.max(this.#late, arrival);
		}
	}

	// An interval whose stalled frames were due before the open stall's last one arrived is still
	// behind that stall, so it joins it instead of counting again: one outage is one observation,
	// as one queue's hundred frames are within an interval.
	#close(bucket: number): void {
		const stalled = this.#late !== Number.NEGATIVE_INFINITY;
		if (stalled && this.#stall !== undefined && this.#due < this.#stallLate) {
			this.#stall = Math.max(this.#stall, bucket);
			this.#span += 1;
			this.#stallLate = this.#late;
			return;
		}

		this.#settle();
		if (stalled) {
			this.#stall = bucket;
			this.#span = 0;
			this.#stallLate = this.#late;
		} else {
			this.#fold(bucket);
		}
	}

	// A stall folds once, its largest delay, and every other interval it spanned is idle, so the
	// estimate keeps its time constant without the stall taking the prior's regained weight.
	#settle(): void {
		if (this.#stall === undefined) return;
		this.#fold(this.#stall);
		for (let i = 0; i < Math.min(this.#span, IDLE_MAX); i++) this.#fold(undefined);
		this.#stall = undefined;
	}

	/**
	 * A declared end lies between the previous observation and the next, so the silence it declares
	 * does not relax the estimate the way an unexplained gap does. Call it before observing the first
	 * frame after the end, not at the marker: the stopped encoder's final packets may still follow it.
	 */
	end(): void {
		// Before any observation there is no interval for the silence to stretch.
		if (this.#origin !== undefined) this.#ended = true;
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

	/** The measured term in milliseconds: the upper edge of the bucket holding the 95th percentile. */
	get measured(): number {
		let cumulative = 0;
		for (let i = 0; i < BUCKETS; i++) {
			cumulative += this.#histogram[i];
			if (cumulative > QUANTILE) return (i + 1) * BUCKET;
		}
		return BUCKETS * BUCKET;
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

/** One arrival in the reference window. */
export interface Entry {
	arrival: number;
	media: number;
}

/**
 * The arrivals inside the reference window, in media order, answering any prefix's fastest arrival
 * in logarithmic time.
 *
 * A reordered frame is measured against the prefix at or before its timestamp, so a peer that sends
 * only reordered frames would otherwise force a scan per frame. Entries live in a ring indexed from
 * `#head`, and a segment tree over the ring's slots holds each node's fastest slot, the older on a
 * tie, as the scan would pick. A query never spans the ring's wrap point within one node, so a node
 * fully inside it is in arrival order.
 *
 * @internal Exported for the equivalence test.
 */
export class History {
	/** The ring's arrivals, its length a power of two that grows up to `MAX_HISTORY`. */
	#arrival = new Float64Array(0);
	/** The ring's media timestamps, slot for slot with `#arrival`. */
	#media = new Float64Array(0);
	/** The slot of the oldest entry. */
	#head = 0;
	/** The number of entries held. */
	#len = 0;
	/** Leaf `capacity + slot` holds `slot`; each parent holds the faster of its children. */
	#tree = new Uint32Array(0);

	/** The entries held. */
	get length(): number {
		return this.#len;
	}

	/** The entry `index` places after the oldest. */
	at(index: number): Entry {
		const slot = (this.#head + index) & (this.#arrival.length - 1);
		return { arrival: this.#arrival[slot], media: this.#media[slot] };
	}

	/** Append an arrival newer than every entry, after dropping those it pushes out of the window. */
	push(arrival: number, media: number): void {
		// Pruning by media time is what makes a timestamp jump self-correcting: everything before
		// it leaves the window at once and the new frame becomes its own reference.
		while (this.#len > 0 && this.#media[this.#head] < media - WINDOW) this.#pop();
		if (this.#len === MAX_HISTORY) this.#pop();
		if (this.#len === this.#arrival.length) this.#grow();

		const capacity = this.#arrival.length;
		const slot = (this.#head + this.#len) & (capacity - 1);
		this.#arrival[slot] = arrival;
		this.#media[slot] = media;
		this.#len++;

		for (let node = (capacity + slot) >> 1; node > 0; node >>= 1) {
			this.#tree[node] = this.#faster(this.#tree[2 * node], this.#tree[2 * node + 1]);
		}
	}

	/** The fastest entry with `entry.media <= media`, the oldest on a tie. */
	reference(media: number): Entry | undefined {
		// Media timestamps increase along the ring, so the prefix ends at a binary search.
		const mask = this.#arrival.length - 1;
		let lo = 0;
		let hi = this.#len;
		while (lo < hi) {
			const mid = (lo + hi) >> 1;
			if (this.#media[(this.#head + mid) & mask] <= media) lo = mid + 1;
			else hi = mid;
		}
		if (lo === 0) return undefined;

		const end = (this.#head + lo - 1) & mask;
		const slot =
			this.#head <= end
				? this.#range(this.#head, end)
				: this.#faster(this.#range(this.#head, mask), this.#range(0, end));
		return { arrival: this.#arrival[slot], media: this.#media[slot] };
	}

	#pop(): void {
		this.#head = (this.#head + 1) & (this.#arrival.length - 1);
		this.#len--;
	}

	/** Double the ring, unwrapping it to start at slot zero, and rebuild the tree. */
	#grow(): void {
		const old = this.#arrival.length;
		const capacity = Math.max(1, old * 2);
		const arrival = new Float64Array(capacity);
		const media = new Float64Array(capacity);
		for (let i = 0; i < this.#len; i++) {
			const slot = (this.#head + i) & (old - 1);
			arrival[i] = this.#arrival[slot];
			media[i] = this.#media[slot];
		}
		this.#arrival = arrival;
		this.#media = media;
		this.#head = 0;

		this.#tree = new Uint32Array(2 * capacity);
		for (let slot = 0; slot < capacity; slot++) this.#tree[capacity + slot] = slot;
		for (let node = capacity - 1; node > 0; node--) {
			this.#tree[node] = this.#faster(this.#tree[2 * node], this.#tree[2 * node + 1]);
		}
	}

	/** The fastest slot in `first..=last`, which must not wrap. */
	#range(first: number, last: number): number {
		const capacity = this.#arrival.length;
		let lo = first + capacity;
		let hi = last + capacity + 1;
		let older = -1;
		let newer = -1;
		while (lo < hi) {
			if (lo & 1) {
				older = older < 0 ? this.#tree[lo] : this.#faster(older, this.#tree[lo]);
				lo++;
			}
			if (hi & 1) {
				hi--;
				newer = newer < 0 ? this.#tree[hi] : this.#faster(this.#tree[hi], newer);
			}
			lo >>= 1;
			hi >>= 1;
		}
		if (older < 0) return newer;
		if (newer < 0) return older;
		return this.#faster(older, newer);
	}

	/** Of two slots, `older` first in arrival order, the faster one, keeping `older` on a tie. */
	#faster(older: number, newer: number): number {
		return this.#arrival[newer] - this.#media[newer] < this.#arrival[older] - this.#media[older] ? newer : older;
	}
}
