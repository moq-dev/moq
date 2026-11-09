/*!
 * Received-PCM adaptation of WebRTC NetEq's accelerate, the shared seams and pitch search
 * doc/concept/audio-jitter.md specifies for this player and moq-audio's. This player never expands:
 * its ring starts on its target, so slowing down only ever settles audio behind the shared clock.
 *
 * Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
 * Ported from modules/audio_coding/neteq/{time_stretch,accelerate,dsp_helper,
 * audio_vector,decision_logic}.
 * @license BSD-3-Clause, with WebRTC's patent grant: see WEBRTC-LICENSE and WEBRTC-PATENTS in this package.
 */

/** A full processing window of received PCM, in milliseconds. */
export const WINDOW = 30;

/** What a window boundary decides, named as the shared corpus names it. */
export type Decision = "Hold" | "Accelerate" | "Fast";

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
	// 20 ms: NetEq's adjustment granularity. An excess this large fast-accelerates.
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
		this.window = Math.max(1, Math.floor((rate * WINDOW) / 1000));
		this.#middle = Math.floor((rate * 15) / 1000);
		this.#band = Math.floor((rate * 20) / 1000);
		this.#rest = Math.floor((rate * 50) / 1000);
		this.cooldown = Math.floor((rate * 60) / 1000);
		this.input = Array.from({ length: channels }, () => new Float32Array(this.window));
		this.#output = Array.from({ length: channels }, () => new Float32Array(this.window));
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
	 * Stage the first `frames` of {@link input}, accelerated by at most `excess` frames: how far the ring
	 * sits behind its target. Only a full window of received PCM can stretch, so a short block, or one
	 * that is not `enabled`, passes unchanged and a finite input never becomes concealment.
	 */
	process(frames: number, excess: number, enabled: boolean): void {
		const decision = enabled && frames === this.window ? this.decide(excess) : "Hold";
		const lag = decision === "Hold" ? 0 : this.#pitch(decision === "Fast", excess);
		this.#position = 0;
		this.#length = frames - lag;
		if (lag === 0) {
			for (let channel = 0; channel < this.input.length; channel++) {
				const input = this.input[channel];
				const output = this.#output[channel];
				for (let i = 0; i < frames; i++) output[i] = input[i];
			}
			return;
		}

		// Remove `lag` frames after the seam, crossfading the period before the middle into the one after.
		const middle = this.#middle;
		const seam = middle - lag;
		for (let channel = 0; channel < this.input.length; channel++) {
			const input = this.input[channel];
			const output = this.#output[channel];
			for (let i = 0; i < seam; i++) output[i] = input[i];
			// NetEq's AudioVector::CrossFade, in floating point. The endpoints stay inside the seam
			// rather than duplicating either neighbour.
			for (let i = 0; i < lag; i++) {
				const weight = (i + 1) / (lag + 1);
				output[seam + i] = input[seam + i] * (1 - weight) + input[middle + i] * weight;
			}
			for (let i = middle + lag; i < frames; i++) output[i - lag] = input[i];
		}
		this.cooldown = this.#rest;
	}

	/**
	 * What a full window decides given the ring's `excess` frames above its target, measured where a
	 * frame has just arrived. Any excess accelerates, so the ring rests on its target rather than
	 * anywhere in a band above it, and one past NetEq's 20 ms granularity fast-accelerates, which
	 * catches up after a refill about as fast as dropping the backlog would. A ring at or below its
	 * target holds: that is either audio on the clock or arrivals running late, and slowing down for
	 * either adds latency.
	 */
	decide(excess: number): Decision {
		if (excess >= this.#band) return "Fast";
		if (this.cooldown > 0 || excess <= 0) return "Hold";
		return "Accelerate";
	}

	// Search the first channel at 4 kHz, then admit the full-rate seam, returning its length or zero.
	// Fast mode admits a weaker match and takes every whole period that fits before the middle. An
	// ordinary accelerate removes one period, and only one that fits in the `excess`, so the ring never
	// lands ahead of its target.
	#pitch(fast: boolean, excess: number): number {
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
		if (fast) return Math.floor(middle / lag) * lag;
		return lag <= excess ? lag : 0;
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
