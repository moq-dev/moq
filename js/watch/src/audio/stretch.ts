/**
 * The received-audio time stretch doc/concept/audio-jitter.md specifies for this player and
 * moq-audio's: a rate proportional to the playhead's lag behind the shared clock, played as
 * waveform-similarity overlap-add (WSOLA) splices, whatever the content.
 */

/** The most the stretch speeds up or slows down, as a fraction of real time. */
const CAP = 0.1;

/**
 * The rate is the lag over this many milliseconds: a lag past 50 ms runs at the cap, and a smaller one
 * decays with this time constant.
 */
const GAIN = 500;

/** What a hop boundary decides on, in frames at the device rate. */
export interface Inputs {
	/** How far the playhead trails the shared clock: negative when it plays ahead of it. */
	lag: number;
	/** Received audio ahead of the playhead, decoded or still encoded. */
	depth: number;
	/** How far audio may trail the clock before it snaps onto it. Zero turns the stretch off. */
	bound: number;
	/** What the device reads at once. */
	read: number;
}

/** How a hop may move the playhead, in frames: a positive shift skips ahead, a negative one repeats. */
export interface Decision {
	/** The rate the playhead moves at, less real time: positive plays faster, zero holds. */
	pace: number;
	/** The least and most one splice may shift the playhead. */
	low: number;
	high: number;
	/** Splice as soon as the cap allows, rather than once the pace adds up to a hop, as audio about to run out must. */
	urgent: boolean;
}

/** Play as received. */
export const HOLD: Decision = { pace: 0, low: 0, high: 0, urgent: false };

/** The frames of one hop at `rate`: 10 ms, the crossfade a splice plays and the spacing of decisions. */
export function hop(rate: number): number {
	return Math.max(1, Math.floor((rate * 10) / 1000));
}

/**
 * The shared decision rule. Audio about to run out, with less received ahead of the playhead than two
 * hops and a device read, slows down at the cap, never trailing the clock past the bound. Otherwise a
 * lag past half a hop moves the playhead toward the clock at a rate proportional to it. A splice moves
 * the playhead at least half a hop, never carries it more than half a hop past the clock, and never
 * speeds up into that cushion.
 */
export function decide(rate: number, inputs: Inputs): Decision {
	const { lag, depth, bound, read } = inputs;
	if (bound <= 0) return HOLD;
	const frames = hop(rate);
	const cushion = 2 * frames + read;
	const least = Math.floor(frames / 2);
	if (depth < cushion && lag < bound) {
		return { pace: -CAP, low: Math.ceil(lag - bound), high: -least, urgent: true };
	}
	if (Math.abs(lag) <= least) return HOLD;
	const pace = Math.min(Math.max((lag * 1000) / (rate * GAIN), -CAP), CAP);
	if (lag > 0) return { pace, low: least, high: Math.floor(Math.min(lag + least, depth - cushion)), urgent: false };
	return { pace, low: Math.ceil(lag - least), high: -least, urgent: false };
}

/**
 * Received PCM on its way out, with the history and lookahead a splice searches, allocated up front
 * so the render callback never allocates.
 *
 * Output copies the input until a hop boundary decides to splice: the next hop then crossfades from
 * the input's natural continuation into the stretch of it most similar to that continuation, a shift
 * of at most `span` either way, and continues from there.
 */
export class Stretch {
	/** One hop: the crossfade a splice plays, and the output between decisions. */
	readonly hop: number;
	/** Two hops: the farthest one splice moves the playhead, and the history kept behind it. */
	readonly span: number;
	/** Three hops: the received audio past the playhead a splice can need. */
	readonly window: number;

	readonly #input: Float32Array[];
	// Input frames held, and the playhead among them. During a crossfade the playhead stays where it
	// started, and moves on past the shifted hop once the crossfade ends.
	#filled = 0;
	#head = 0;
	// The crossfade in progress, a zero shift for none, and its frames played.
	#shift = 0;
	#faded = 0;
	// How far the pace has moved the playhead that no splice took yet, and which way it moves.
	#owed = 0;
	#direction = 0;
	// How far splices may still move the playhead: refilled at the cap each hop, up to a span.
	#allowance = 0;
	// Output frames until the next hop boundary.
	#until = 0;

	// The crossfade's weights, and the analysis scratch: the running energy across every channel.
	readonly #weights: Float64Array;
	readonly #energy: Float64Array;
	// The coarse search's stride: every shift on an 8 kHz grid.
	readonly #step: number;

	constructor(rate: number, channels: number) {
		this.hop = hop(rate);
		this.span = 2 * this.hop;
		this.window = 3 * this.hop;
		// The history, the window, and a hop of room to top it up.
		this.#input = Array.from({ length: channels }, () => new Float32Array(this.span + this.window + this.hop));
		this.#allowance = this.span;
		this.#weights = new Float64Array(this.hop);
		for (let k = 0; k < this.hop; k++)
			this.#weights[k] = 0.5 - 0.5 * Math.cos((Math.PI * (k + 1)) / (this.hop + 1));
		this.#energy = new Float64Array(2 * this.span + this.hop + 1);
		this.#step = Math.max(1, Math.floor(rate / 8000));
	}

	/** Whether a hop boundary is next, so {@link next} must decide before anything more plays. */
	get due(): boolean {
		return this.#until === 0;
	}

	/** Output frames left in what the stretch holds, as no further splice changes them. */
	get available(): number {
		return this.#filled - this.#head - this.#shift - this.#faded;
	}

	/** Input frames past the playhead, with a crossfade's played share mapped onto the shifted input. */
	get ahead(): number {
		const moved = this.#shift === 0 ? 0 : (this.#faded * (this.hop + this.#shift)) / this.hop;
		return this.#filled - this.#head - moved;
	}

	/** Input frames past where the playhead, or the crossfade it plays, starts. */
	get queued(): number {
		return this.#filled - this.#head;
	}

	/** Input frames the stretch has room for, keeping its history. */
	room(): number {
		this.#compact(this.span);
		return this.#input[0].length - this.#filled;
	}

	/** Append `frames` from `source`'s planar circular buffers, starting at index `from`. */
	push(source: Float32Array[], from: number, frames: number): void {
		// Index loops rather than `set(subarray(...))`, since each `subarray` allocates a view.
		for (let channel = 0; channel < this.#input.length; channel++) {
			const src = source[channel];
			const dst = this.#input[channel];
			for (let i = 0; i < frames; i++) dst[this.#filled + i] = src[(from + i) % src.length];
		}
		this.#filled += frames;
	}

	/** Drop the history, so no splice reaches back across a boundary in what the ring holds. */
	forget(): void {
		this.#compact(0);
	}

	/** Drop everything held, a crossfade in progress included. */
	clear(): void {
		this.#filled = 0;
		this.#head = 0;
		this.#shift = 0;
		this.#faded = 0;
		this.#owed = 0;
		this.#direction = 0;
		this.#allowance = this.span;
		this.#until = 0;
	}

	/**
	 * Decide the hop starting now, returning the shift a splice took, or zero. The pace adds up until it
	 * reaches a hop, or at once when urgent, and a splice then takes the most similar shift within a hop
	 * of that, inside the decision's range, the cap's allowance, and what the stretch holds.
	 */
	next(decision: Decision): number {
		this.#until = this.hop;
		this.#allowance = Math.min(this.#allowance + CAP * this.hop, this.span);
		const direction = Math.sign(decision.pace);
		if (direction !== this.#direction) {
			this.#direction = direction;
			this.#owed = 0;
		}
		if (direction === 0) return 0;
		this.#owed = Math.min(Math.max(this.#owed + decision.pace * this.hop, -this.span), this.span);
		if (!decision.urgent && direction * this.#owed < this.hop) return 0;

		// The crossfade reads a hop of natural continuation and a hop past the shift.
		const ahead = this.#filled - this.#head - this.hop;
		if (ahead < 0) return 0;
		const allowed = Math.floor(this.#allowance);
		let low = Math.max(decision.low, -allowed, -this.#head);
		let high = Math.min(decision.high, allowed, ahead);
		if (!decision.urgent) {
			low = Math.max(low, Math.ceil(this.#owed) - this.hop);
			high = Math.min(high, Math.floor(this.#owed) + this.hop);
		}
		const shift = this.#search(low, high, direction);
		if (shift === 0) return 0;
		this.#shift = shift;
		this.#faded = 0;
		this.#owed = Math.min(Math.max(this.#owed - shift, -this.span), this.span);
		this.#allowance -= Math.abs(shift);
		return shift;
	}

	/** Play up to `frames` into `output` from `offset`, stopping at the next hop boundary, and return how many. */
	read(output: Float32Array[], offset: number, frames: number): number {
		let done = 0;
		while (done < frames && this.#until > 0) {
			const at = offset + done;
			if (this.#shift !== 0) {
				const count = Math.min(frames - done, this.#until);
				const from = this.#head + this.#faded;
				const to = from + this.#shift;
				for (let channel = 0; channel < output.length; channel++) {
					const src = this.#input[channel];
					const dst = output[channel];
					for (let i = 0; i < count; i++) {
						const weight = this.#weights[this.#faded + i];
						dst[at + i] = src[from + i] * (1 - weight) + src[to + i] * weight;
					}
				}
				this.#faded += count;
				this.#until -= count;
				done += count;
				if (this.#faded === this.hop) {
					this.#head += this.#shift + this.hop;
					this.#shift = 0;
					this.#faded = 0;
				}
				continue;
			}
			const count = Math.min(frames - done, this.#until, this.#filled - this.#head);
			if (count === 0) break;
			for (let channel = 0; channel < output.length; channel++) {
				const src = this.#input[channel];
				const dst = output[channel];
				for (let i = 0; i < count; i++) dst[at + i] = src[this.#head + i];
			}
			this.#head += count;
			this.#until -= count;
			done += count;
		}
		return done;
	}

	// Keep at most `history` frames behind the playhead.
	#compact(history: number): void {
		const drop = this.#head - history;
		if (drop <= 0) return;
		for (const channel of this.#input) channel.copyWithin(0, drop, this.#filled);
		this.#head -= drop;
		this.#filled -= drop;
	}

	// The shift in [low, high] whose hop best matches the natural continuation, by normalized correlation
	// over every channel's samples together, so channels in opposite phase never cancel. Nearest first,
	// so a tie keeps the smaller shift: every shift on the coarse grid, then every one within a step of
	// the best. Zero when the range is empty.
	#search(low: number, high: number, direction: number): number {
		if (low > high) return 0;
		const hop = this.hop;
		const input = this.#input;
		const from = this.#head + Math.min(low, 0);
		const length = Math.max(high, 0) + hop - Math.min(low, 0);
		const energy = this.#energy;
		for (let i = 0; i < length; i++) {
			let sum = 0;
			for (const channel of input) sum += channel[from + i] * channel[from + i];
			energy[i + 1] = energy[i] + sum;
		}
		const head = this.#head;
		const natural = head - from;
		const power = energy[natural + hop] - energy[natural];
		const similarity = (shift: number) => {
			const at = natural + shift;
			let dot = 0;
			for (let k = 0; k < hop; k++) {
				for (const channel of input) dot += channel[head + k] * channel[head + shift + k];
			}
			const product = power * (energy[at + hop] - energy[at]);
			return product > 0 ? dot / Math.sqrt(product) : 0;
		};

		const near = direction > 0 ? low : high;
		const far = direction > 0 ? high : low;
		const step = this.#step;
		let best = near;
		let score = Number.NEGATIVE_INFINITY;
		for (let shift = near; direction * (far - shift) >= 0; shift += direction * step) {
			const value = similarity(shift);
			if (value > score) {
				score = value;
				best = shift;
			}
		}
		const start = direction > 0 ? Math.max(low, best - step + 1) : Math.min(high, best + step - 1);
		const end = direction > 0 ? Math.min(high, best + step - 1) : Math.max(low, best - step + 1);
		score = Number.NEGATIVE_INFINITY;
		for (let shift = start; direction * (end - shift) >= 0; shift += direction) {
			const value = similarity(shift);
			if (value > score) {
				score = value;
				best = shift;
			}
		}
		return best;
	}
}
