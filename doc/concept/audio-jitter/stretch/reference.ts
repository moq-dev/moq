// A reference implementation of the received-audio time stretch described in
// /concept/audio-jitter, used only to generate the conformance corpus beside it.
//
// It is not normative and is not shipped in any package. The document is normative:
// when the two disagree the document wins, this file is corrected, and the corpus is
// regenerated. Nothing outside this directory may import it, because an
// implementation that calls the reference proves nothing about the document.
//
// It keeps the whole input and plays it from absolute positions, where the players
// keep a ring of history and lookahead, so the two share no structure to be wrong in
// the same way.

/** The most the stretch speeds up or slows down, as a fraction of real time. */
export const CAP = 0.1;

/** The rate is the lag over this many milliseconds, so it reaches the cap at 50 ms. */
export const GAIN = 500;

/** What a hop boundary decides on, in frames. */
export interface Inputs {
	lag: number;
	depth: number;
	bound: number;
	read: number;
}

/** How a hop may move the playhead, in frames. */
export interface Decision {
	pace: number;
	low: number;
	high: number;
	urgent: boolean;
}

export const HOLD: Decision = { pace: 0, low: 0, high: 0, urgent: false };

/** 10 ms of frames, at least one. */
export function hop(rate: number): number {
	return Math.max(1, Math.floor((rate * 10) / 1000));
}

/** The decision rule, read straight off the document. */
export function decide(rate: number, { lag, depth, bound, read }: Inputs): Decision {
	if (bound <= 0) return HOLD;
	const h = hop(rate);
	const cushion = 2 * h + read;
	const least = Math.floor(h / 2);
	if (depth < cushion && lag < bound) {
		return { pace: -CAP, low: Math.ceil(lag - bound), high: -least, urgent: true };
	}
	if (Math.abs(lag) <= least) return HOLD;
	const pace = Math.min(Math.max((lag * 1000) / (rate * GAIN), -CAP), CAP);
	if (lag > 0) return { pace, low: least, high: Math.floor(Math.min(lag + least, depth - cushion)), urgent: false };
	return { pace, low: Math.ceil(lag - least), high: -least, urgent: false };
}

const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

/**
 * Play `input` (interleaved) hop by hop, the decision for hop `i` taken from `decisions[i]`, the last
 * one repeated, with the input past the playhead capped at three hops the way a player tops it up.
 * Returns the interleaved output and the shift each hop took.
 */
export function play(
	rate: number,
	channels: number,
	input: Float32Array,
	decisions: Decision[],
): { output: Float32Array; shifts: number[] } {
	const h = hop(rate);
	const span = 2 * h;
	const window = 3 * h;
	const frames = input.length / channels;
	const weight = (k: number) => 0.5 - 0.5 * Math.cos((Math.PI * (k + 1)) / (h + 1));
	const sample = (frame: number, channel: number) => input[frame * channels + channel];

	const out: number[] = [];
	const shifts: number[] = [];
	let playhead = 0;
	let owed = 0;
	let direction = 0;
	let allowance = span;

	for (let index = 0; playhead < frames; index++) {
		const decision = decisions[Math.min(index, decisions.length - 1)];
		const queued = Math.min(window, frames - playhead);
		allowance = Math.min(allowance + CAP * h, span);
		let shift = 0;
		const sign = Math.sign(decision.pace);
		if (sign !== direction) {
			direction = sign;
			owed = 0;
		}
		if (sign !== 0) {
			owed = clamp(owed + decision.pace * h, -span, span);
			if ((decision.urgent || sign * owed >= h) && queued >= h) {
				const allowed = Math.floor(allowance);
				let low = Math.max(decision.low, -allowed, -Math.min(playhead, span));
				let high = Math.min(decision.high, allowed, queued - h);
				if (!decision.urgent) {
					low = Math.max(low, Math.ceil(owed) - h);
					high = Math.min(high, Math.floor(owed) + h);
				}
				shift = search(low, high, sign);
				if (shift !== 0) {
					owed = clamp(owed - shift, -span, span);
					allowance -= Math.abs(shift);
				}
			}
		}
		shifts.push(shift);

		if (shift === 0) {
			const count = Math.min(h, frames - playhead);
			for (let k = 0; k < count; k++) {
				for (let channel = 0; channel < channels; channel++) out.push(sample(playhead + k, channel));
			}
			playhead += count;
		} else {
			for (let k = 0; k < h; k++) {
				const w = weight(k);
				for (let channel = 0; channel < channels; channel++) {
					out.push(sample(playhead + k, channel) * (1 - w) + sample(playhead + shift + k, channel) * w);
				}
			}
			playhead += shift + h;
		}
	}
	return { output: Float32Array.from(out), shifts };

	// Normalized correlation of the hop at the playhead with the hop at `shift`, every channel's samples
	// together, so channels in opposite phase never cancel, from energies summed directly over each hop.
	function similarity(shift: number): number {
		let dot = 0;
		let a = 0;
		let b = 0;
		for (let k = 0; k < h; k++) {
			for (let channel = 0; channel < channels; channel++) {
				const x = sample(playhead + k, channel);
				const y = sample(playhead + shift + k, channel);
				dot += x * y;
				a += x * x;
				b += y * y;
			}
		}
		const product = a * b;
		return product > 0 ? dot / Math.sqrt(product) : 0;
	}

	// The most similar shift in [low, high], nearest the playhead first so a tie keeps the nearer one:
	// every eighth of a millisecond, then each shift within that stride of the best.
	function search(low: number, high: number, sign: number): number {
		if (low > high) return 0;
		const step = Math.max(1, Math.floor(rate / 8000));
		const near = sign > 0 ? low : high;
		const nearer = (a: number, b: number) => Math.abs(a - near) < Math.abs(b - near);
		// A best that beats the next by less than rounding could tell apart is an ambiguous vector:
		// the players sum energies another way, so the generator refuses it rather than encode either.
		const pick = (candidates: number[]) => {
			let best = candidates[0];
			let score = similarity(best);
			let runner = Number.NEGATIVE_INFINITY;
			for (const shift of candidates.slice(1)) {
				const value = similarity(shift);
				if (value > score || (value === score && nearer(shift, best))) {
					runner = score;
					score = value;
					best = shift;
				} else {
					runner = Math.max(runner, value);
				}
			}
			if (score - runner < 1e-9 && (score !== 0 || runner !== 0))
				throw new Error(`ambiguous splice at ${playhead}: ${score} vs ${runner}`);
			return best;
		};
		const coarse: number[] = [];
		for (let k = 0; near + sign * k * step >= low && near + sign * k * step <= high; k++) {
			coarse.push(near + sign * k * step);
		}
		const best = pick(coarse);
		const fine: number[] = [];
		for (let shift = Math.max(low, best - step + 1); shift <= Math.min(high, best + step - 1); shift++) {
			fine.push(shift);
		}
		fine.sort((a, b) => Math.abs(a - near) - Math.abs(b - near));
		return pick(fine);
	}
}
