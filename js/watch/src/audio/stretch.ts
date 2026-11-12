/*!
 * Received-PCM adaptation of WebRTC NetEq's accelerate and preemptive expand: the decision, buffer-level
 * filter, seams, and pitch search doc/concept/audio-jitter.md specifies for this player and moq-audio's.
 *
 * Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
 * Ported from modules/audio_coding/neteq/{time_stretch,accelerate,preemptive_expand,dsp_helper,
 * audio_vector,decision_logic,buffer_level_filter}.
 * @license BSD-3-Clause, with WebRTC's patent grant: see WEBRTC-LICENSE and WEBRTC-PATENTS in this package.
 */

/** What a window boundary decides, named as the shared corpus names it. */
export type Decision = "Hold" | "Accelerate" | "Fast" | "Expand";

/** What a window decides on, in frames at the device rate. */
export interface Inputs {
	/** The playout delay the shared clock plays at. */
	target: number;
	/**
	 * How far the playhead trailed the newest received audio when it arrived, less what the stretch
	 * removed since: the buffer level with the publisher's cadence taken out. Filtered before deciding.
	 */
	level: number;
	/** Received audio ahead of the playhead, decoded or not. */
	raw: number;
	/** How far the playhead trails the shared clock: negative when it plays ahead of it. */
	lag: number;
	/** How far behind the clock audio may fall: the subscription's max delay less the playout delay. */
	bound: number;
}

/** A decision and the most a seam may change the output by in frames: zero when it holds. */
export interface Verdict {
	decision: Decision;
	allowance: number;
}

/**
 * The shared decision rule, on a filtered `level`. Fast accelerate and accelerate only ever catch a
 * playhead behind the clock up to it; expand slows one whose buffer runs low, as far as `bound` allows,
 * or one ahead of the clock back onto it. `min(level, raw)` is the buffer: a stall delivers no arrival
 * to lower `level`, so `raw` stops a playhead behind the clock from speeding into it.
 */
export function decide(rate: number, inputs: Inputs, cooldown: number): Verdict {
	const { target, level, raw, lag, bound } = inputs;
	// NetEq's DecisionLogic limits, with its 85 ms deceleration offset and 20 ms granularity.
	const low = Math.max(Math.floor((3 * target) / 4), target - Math.floor((rate * 85) / 1000));
	const high = Math.max(target, low + Math.floor((rate * 20) / 1000));
	const buffer = Math.min(level, raw);
	if (lag > 0 && buffer >= 4 * high) return { decision: "Fast", allowance: lag };
	if (cooldown > 0) return { decision: "Hold", allowance: 0 };
	if (lag > 0 && buffer >= high) return { decision: "Accelerate", allowance: lag };
	if (level < low && lag < bound) return { decision: "Expand", allowance: bound - lag };
	if (lag < 0) return { decision: "Expand", allowance: -lag };
	return { decision: "Hold", allowance: 0 };
}

/**
 * NetEq's buffer-level filter, updated once per window: three of its 10 ms steps, its factor chosen
 * by the target, less what the stretch changed since the last update. Unset until the first update
 * after a start or a {@link reset}, which takes the level as it is.
 */
export class Level {
	#filtered?: number;

	/** The filtered level, unset before the first update. */
	get filtered(): number | undefined {
		return this.#filtered;
	}

	/** Fold in `level`, after a window whose stretch removed `stretched` frames (negative: added). */
	update(rate: number, target: number, level: number, stretched: number): number {
		const filtered = this.#filtered;
		if (filtered === undefined) {
			this.#filtered = level;
			return level;
		}
		// NetEq's BufferLevelFilter factors, in 256ths per 10 ms step, by the target in milliseconds.
		const factor =
			target * 1000 <= 20 * rate
				? 251
				: target * 1000 <= 60 * rate
					? 252
					: target * 1000 <= 140 * rate
						? 253
						: 254;
		const weight = (factor * factor * factor) / 16777216;
		this.#filtered = Math.max(0, weight * filtered + (1 - weight) * level - stretched);
		return this.#filtered;
	}

	/** Forget the history, so the next update starts from its level: after an underrun or a new timeline. */
	reset(): void {
		this.#filtered = undefined;
	}
}

const HOLD: Verdict = { decision: "Hold", allowance: 0 };

// NetEq's 48 kHz Q12 FIR. Sampling its taps at fractional positions also admits other device rates
// without altering the search domain.
const FILTER = [1019, 390, 427, 440, 427, 390, 1019];

// The 4 kHz analysis: 110 downsampled samples, correlated over 50 at lags 10 through 59.
const DOWN = 110;
const LAGS = 50;
const MIN_LAG = 10;

// NetEq's voice activity bound before it has a background noise model, from signed 16-bit PCM.
const PASSIVE = (8 * 75000) / (32768 * 32768);

/**
 * One window of received PCM and the stretched output staged from it, allocated up front so the
 * render callback never allocates.
 */
export class Stretch {
	/** The window to analyse, one array per channel: fill the first `frames` of each before {@link process}. */
	readonly input: Float32Array[];
	readonly #output: Float32Array[];

	/** A full processing window: 30 ms of received PCM, in frames. */
	readonly window: number;

	readonly #rate: number;
	// 15 ms: the seam's position, and the longest pitch period.
	readonly #middle: number;
	// 50 ms: NetEq's five 10 ms ticks between stretches.
	readonly #rest: number;

	#position = 0;
	#length = 0;
	// What the stretch removed since the last filter update, which that update takes out of its history.
	#stretched = 0;

	/** Frames left to render before an ordinary stretch: NetEq's six startup ticks, then its five. */
	cooldown: number;

	/** The buffer-level filter the decisions read. */
	readonly level = new Level();

	readonly #down = new Float64Array(DOWN);
	readonly #correlation = new Float64Array(LAGS);

	constructor(rate: number, channels: number) {
		this.#rate = rate;
		this.window = Math.max(1, Math.floor((rate * 30) / 1000));
		this.#middle = Math.floor((rate * 15) / 1000);
		this.#rest = Math.floor((rate * 50) / 1000);
		this.cooldown = Math.floor((rate * 60) / 1000);
		this.input = Array.from({ length: channels }, () => new Float32Array(this.window));
		// An expand adds up to one 15 ms period.
		this.#output = Array.from({ length: channels }, () => new Float32Array(this.window + this.#middle));
	}

	/** Output frames staged and not yet read. */
	get available(): number {
		return this.#length - this.#position;
	}

	/** Output frames the last window produced, read or not. */
	get length(): number {
		return this.#length;
	}

	/** Drop whatever is staged. */
	clear(): void {
		this.#position = 0;
		this.#length = 0;
	}

	/** Copy staged frames into `output` from `offset`, as many as fit, and return how many. */
	read(output: Float32Array[], offset: number): number {
		const frames = Math.min(output[0].length - offset, this.available);
		// Index loops rather than `set(subarray(...))`, since each `subarray` allocates a view.
		for (let channel = 0; channel < output.length; channel++) {
			const src = this.#output[channel];
			const dst = output[channel];
			for (let i = 0; i < frames; i++) dst[offset + i] = src[this.#position + i];
		}
		this.#position += frames;
		this.cooldown = Math.max(0, this.cooldown - frames);
		return frames;
	}

	/**
	 * Stage the first `frames` of {@link input}, stretched as `inputs` decide. Only a full window of
	 * received PCM can stretch, so a short block, or one with no `inputs`, passes unchanged and a
	 * finite input never becomes concealment.
	 */
	process(frames: number, inputs?: Inputs): Decision {
		if (!inputs || frames !== this.window) return this.stage(frames, HOLD);
		const level = this.level.update(this.#rate, inputs.target, inputs.level, this.#stretched);
		this.#stretched = 0;
		return this.stage(frames, decide(this.#rate, { ...inputs, level }, this.cooldown));
	}

	/** Stage the first `frames` of {@link input} as `verdict` decides, if a pitch period admits it. */
	stage(frames: number, verdict: Verdict): Decision {
		const { decision } = verdict;
		const lag = decision === "Hold" ? 0 : this.#pitch(decision, verdict.allowance);
		this.#position = 0;
		const expand = decision === "Expand";
		this.#length = expand ? frames + lag : frames - lag;
		this.#stretched += frames - this.#length;
		if (lag === 0) {
			for (let channel = 0; channel < this.input.length; channel++) {
				const input = this.input[channel];
				const output = this.#output[channel];
				for (let i = 0; i < frames; i++) output[i] = input[i];
			}
			return decision;
		}

		// Accelerate removes `lag` frames after the seam, crossfading the period before the middle into
		// the one after; expand adds them, crossfading the period after the middle into the one before.
		const middle = this.#middle;
		const seam = expand ? middle : middle - lag;
		const first = expand ? middle : middle - lag;
		const second = expand ? middle - lag : middle;
		const tail = expand ? middle : middle + lag;
		for (let channel = 0; channel < this.input.length; channel++) {
			const input = this.input[channel];
			const output = this.#output[channel];
			for (let i = 0; i < seam; i++) output[i] = input[i];
			// NetEq's AudioVector::CrossFade, in floating point. The endpoints stay inside the seam
			// rather than duplicating either neighbour.
			for (let i = 0; i < lag; i++) {
				const weight = (i + 1) / (lag + 1);
				output[seam + i] = input[first + i] * (1 - weight) + input[second + i] * weight;
			}
			for (let i = tail; i < frames; i++) output[seam + lag + i - tail] = input[i];
		}
		this.cooldown = this.#rest;
		return decision;
	}

	// Search the first channel at 4 kHz, then admit the full-rate seam, returning how many frames it
	// changes the output by, or zero. Fast mode admits a weaker match and takes every whole period that
	// fits before the middle; the others take one. None takes more than `allowance`.
	#pitch(decision: Decision, allowance: number): number {
		const middle = this.#middle;
		if (middle === 0) return 0;

		const rate = this.#rate;
		const down = this.#down;
		for (let i = 0; i < DOWN; i++) {
			let value = 0;
			for (let tap = 0; tap < FILTER.length; tap++) {
				value += (FILTER[tap] * this.#sample(((10 + 12 * i - tap) * rate) / 48000)) / 4096;
			}
			down[i] = value;
		}

		const correlation = this.#correlation;
		for (let index = 0; index < LAGS; index++) {
			const lag = index + MIN_LAG;
			let value = 0;
			for (let t = 60; t < DOWN; t++) value += down[t] * down[t - lag];
			correlation[index] = value;
		}

		// The largest sum, the smallest lag on a tie, refined by a parabola through its neighbours.
		let peak = 0;
		for (let index = 1; index < LAGS; index++) {
			if (correlation[index] > correlation[peak]) peak = index;
		}
		let offset = 0;
		if (peak !== 0) {
			const left = correlation[peak - 1];
			const center = correlation[peak];
			const right = peak + 1 < LAGS ? correlation[peak + 1] : 0;
			const denominator = left - 2 * center + right;
			if (denominator !== 0) offset = Math.min(Math.max((0.5 * (left - right)) / denominator, -0.5), 0.5);
		}
		const lag = Math.min(Math.max(Math.round(((peak + MIN_LAG + offset) * rate) / 4000), 1), middle);

		const input = this.input[0];
		let energy1 = 0;
		let energy2 = 0;
		let cross = 0;
		for (let i = 0; i < lag; i++) {
			const a = input[middle - lag + i];
			const b = input[middle + i];
			energy1 += a * a;
			energy2 += b * b;
			cross += a * b;
		}
		const fast = decision === "Fast";
		const passive = (energy1 + energy2) / (2 * lag) <= PASSIVE;
		const threshold = (fast ? 8192 : 14746) / 16384;
		if (!passive && cross <= threshold * Math.sqrt(energy1 * energy2)) return 0;
		const periods = fast
			? Math.min(Math.floor(middle / lag), Math.floor(allowance / lag))
			: allowance >= lag
				? 1
				: 0;
		return periods * lag;
	}

	// The first channel at a fractional frame, linearly interpolated and clamped to the window.
	#sample(position: number): number {
		const input = this.input[0];
		const last = this.window - 1;
		const clamped = Math.min(Math.max(position, 0), last);
		const at = Math.floor(clamped);
		const weight = clamped - at;
		return input[at] * (1 - weight) + input[Math.min(at + 1, last)] * weight;
	}
}
