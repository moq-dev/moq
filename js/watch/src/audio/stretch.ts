/*!
 * Received-PCM adaptation of WebRTC NetEq's accelerate and preemptive expand, the shared algorithm
 * doc/concept/audio-jitter.md specifies for this player and moq-audio's.
 *
 * Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
 * Ported from modules/audio_coding/neteq/{time_stretch,accelerate,preemptive_expand,dsp_helper,
 * audio_vector,decision_logic}.
 * @license BSD-3-Clause, with WebRTC's patent grant: see WEBRTC-LICENSE and WEBRTC-PATENTS in this package.
 */

/** What a window boundary decides, named as the shared corpus names it. */
export type Decision = "Hold" | "Accelerate" | "Fast" | "Expand";

// NetEq's 48 kHz Q12 FIR. Sampling its taps at fractional positions also admits other device rates
// without altering the search domain.
const FILTER = [1019, 390, 427, 440, 427, 390, 1019];

// The 4 kHz analysis: 110 downsampled samples, correlated over 50 at lags 10 through 59.
const DOWN = 110;
const LAGS = 50;
const MIN_LAG = 10;

// Below the target, expand every window, as many whole periods as fit, at the lower admission
// threshold. False is NetEq's exact preemptive expand. The maintainer's listening check picks one,
// and native's FAST_EXPAND must match it.
const FAST_EXPAND = true;

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
	// 20 ms: the band above the target that holds before accelerating.
	readonly #band: number;
	// 50 ms: NetEq's five 10 ms ticks between stretches.
	readonly #rest: number;

	#position = 0;
	#length = 0;

	/** Frames left to render before an ordinary stretch: NetEq's six startup ticks, then its five. */
	cooldown: number;

	readonly #down = new Float64Array(DOWN);
	readonly #correlation = new Float64Array(LAGS);

	constructor(rate: number, channels: number) {
		this.#rate = rate;
		this.window = Math.max(1, Math.floor((rate * 30) / 1000));
		this.#middle = Math.floor((rate * 15) / 1000);
		this.#band = Math.floor((rate * 20) / 1000);
		this.#rest = Math.floor((rate * 50) / 1000);
		this.cooldown = Math.floor((rate * 60) / 1000);
		this.input = Array.from({ length: channels }, () => new Float32Array(this.window));
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
		for (let channel = 0; channel < output.length; channel++) {
			output[channel].set(this.#output[channel].subarray(this.#position, this.#position + frames), offset);
		}
		this.#position += frames;
		this.cooldown = Math.max(0, this.cooldown - frames);
		return frames;
	}

	/**
	 * Stage the first `frames` of {@link input}, stretched toward `target` given `buffered` frames
	 * queued including them. Only a full window of received PCM can stretch, so a short block, or one
	 * that is not `enabled`, passes unchanged and a finite input never becomes concealment.
	 */
	process(frames: number, buffered: number, target: number, enabled: boolean): void {
		const decision = enabled && frames === this.window ? this.decide(buffered, target) : "Hold";
		const lag =
			decision === "Hold" ? 0 : this.#pitch(decision === "Fast" || (FAST_EXPAND && decision === "Expand"));
		this.#position = 0;
		this.#length = frames;
		if (lag === 0) {
			for (let channel = 0; channel < this.input.length; channel++) {
				this.#output[channel].set(this.input[channel].subarray(0, frames));
			}
			return;
		}

		// Expand repeats whole periods after the seam; accelerate removes one, or fast as many as fit.
		const middle = this.#middle;
		const expand = decision === "Expand";
		const seam = expand ? middle : middle - lag;
		const first = expand ? middle : middle - lag;
		const second = expand ? middle - lag : middle;
		const tail = expand ? middle : middle + lag;
		this.#length += expand ? lag : -lag;
		for (let channel = 0; channel < this.input.length; channel++) {
			const input = this.input[channel];
			const output = this.#output[channel];
			output.set(input.subarray(0, seam));
			// NetEq's AudioVector::CrossFade, in floating point. The endpoints stay inside the seam
			// rather than duplicating either neighbour.
			for (let i = 0; i < lag; i++) {
				const weight = (i + 1) / (lag + 1);
				output[seam + i] = input[first + i] * (1 - weight) + input[second + i] * weight;
			}
			output.set(input.subarray(tail, frames), seam + lag);
		}
		this.cooldown = this.#rest;
	}

	/**
	 * What a full window with `buffered` frames queued decides against `target`, both in frames.
	 *
	 * With {@link FAST_EXPAND}, expand departs from NetEq's preemptive expand, which repeats one
	 * period per cooldown. A ring below its target is short of the cushion its arrivals need, and
	 * running dry re-stalls it for a whole target of silence, so it expands the way fast accelerate
	 * shrinks: every window, as many whole periods as fit, at the lower admission threshold.
	 */
	decide(buffered: number, target: number): Decision {
		const high = target + this.#band;
		if (buffered >= high * 4) return "Fast";
		if (FAST_EXPAND && buffered < target) return "Expand";
		if (this.cooldown > 0) return "Hold";
		if (buffered >= high) return "Accelerate";
		if (buffered < target) return "Expand";
		return "Hold";
	}

	// Search the first channel at 4 kHz, then admit the full-rate seam, returning its length or zero.
	// Fast mode, for fast accelerate and every expand, admits a weaker match and takes every whole
	// period that fits before the middle.
	#pitch(fast: boolean): number {
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
		const passive = (energy1 + energy2) / (2 * lag) <= PASSIVE;
		const threshold = (fast ? 8192 : 14746) / 16384;
		if (!passive && cross <= threshold * Math.sqrt(energy1 * energy2)) return 0;
		return fast ? Math.floor(middle / lag) * lag : lag;
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
