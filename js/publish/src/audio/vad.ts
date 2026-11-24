/*
 * A port of SILK's voice activity detector (silk/VAD.c, silk/ana_filt_bank_1.c, silk/sigm_Q15.c,
 * silk/lin2log.c) from libopus 1.6.1.
 *
 * Copyright (c) 2006-2011, Skype Limited. All rights reserved.
 * Redistribution and use in source and binary forms, with or without modification, are permitted
 * provided that the following conditions are met:
 * - Redistributions of source code must retain the above copyright notice, this list of
 *   conditions and the following disclaimer.
 * - Redistributions in binary form must reproduce the above copyright notice, this list of
 *   conditions and the following disclaimer in the documentation and/or other materials provided
 *   with the distribution.
 * - Neither the name of Internet Society, IETF or IETF Trust, nor the names of specific
 *   contributors, may be used to endorse or promote products derived from this software without
 *   specific prior written permission.
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
 * FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
 * CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
 * IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
 * OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

import type { AudioFrame } from "./capture";

// The fixed-point arithmetic is SILK's, bit for bit, so the decisions match libopus's on the same
// samples. Each helper is the silk macro of the same name on int32 values.
const INT32_MAX = 0x7fffffff;
const int16 = (x: number) => (x << 16) >> 16;
const smulwb = (a: number, b: number) => Math.floor((a * int16(b)) / 65536);
const smlawb = (a: number, b: number, c: number) => (a + smulwb(b, c)) | 0;
const smulbb = (a: number, b: number) => Math.imul(int16(a), int16(b));
const rshiftRound = (a: number, shift: number) => (shift === 1 ? (a >> 1) + (a & 1) : ((a >> (shift - 1)) + 1) >> 1);
const smulww = (a: number, b: number) => (smulwb(a, b) + Math.imul(a, rshiftRound(b, 16))) | 0;
const sat16 = (a: number) => Math.min(32767, Math.max(-32768, a));
const div32 = (a: number, b: number) => Math.trunc(a / b);
// Both operands are non-negative.
const addPosSat32 = (a: number, b: number) => Math.min(a + b, INT32_MAX);

// Leading zeros, and the 7 bits after the leading one.
function clzFrac(x: number): [number, number] {
	const lz = Math.clz32(x);
	const rot = 24 - lz;
	const u = x >>> 0;
	const rotated = rot === 0 ? u : rot < 0 ? (u << -rot) | (u >>> (32 + rot)) : (u << (32 - rot)) | (u >>> rot);
	return [lz, rotated & 0x7f];
}

function sqrtApprox(x: number): number {
	if (x <= 0) return 0;
	const [lz, frac] = clzFrac(x);
	let y = lz & 1 ? 32768 : 46214;
	y >>= lz >> 1;
	return smlawb(y, y, smulbb(213, frac));
}

// 128 * log2(x), approximately.
function lin2log(x: number): number {
	const [lz, frac] = clzFrac(x);
	return (smlawb(frac, Math.imul(frac, 128 - frac), 179) + ((31 - lz) << 7)) | 0;
}

const SIGM_SLOPE_Q10 = [237, 153, 73, 30, 12, 7];
const SIGM_POS_Q15 = [16384, 23955, 28861, 31213, 32178, 32548];
const SIGM_NEG_Q15 = [16384, 8812, 3906, 1554, 589, 219];

function sigmQ15(x: number): number {
	if (x < 0) {
		x = -x;
		if (x >= 6 * 32) return 0;
		const i = x >> 5;
		return SIGM_NEG_Q15[i] - smulbb(SIGM_SLOPE_Q10[i], x & 0x1f);
	}
	if (x >= 6 * 32) return 32767;
	const i = x >> 5;
	return SIGM_POS_Q15[i] + smulbb(SIGM_SLOPE_Q10[i], x & 0x1f);
}

const A_FB1_20 = 5394 << 1;
const A_FB1_21 = -24290;

// silk_ana_filt_bank_1: split `n` samples of `src` from `at` into decimated low and high bands
// written to `dst`, with first-order allpass filters. The low band may overwrite the input in place,
// since it trails the read.
function anaFiltBank1(
	src: Int16Array,
	at: number,
	state: Int32Array,
	dst: Int16Array,
	low: number,
	high: number,
	n: number,
): void {
	for (let k = 0; k < n >> 1; k++) {
		let in32 = src[at + 2 * k] << 10;
		let y = (in32 - state[0]) | 0;
		let x = smlawb(y, y, A_FB1_21);
		const out1 = (state[0] + x) | 0;
		state[0] = (in32 + x) | 0;

		in32 = src[at + 2 * k + 1] << 10;
		y = (in32 - state[1]) | 0;
		x = smulwb(y, A_FB1_20);
		const out2 = (state[1] + x) | 0;
		state[1] = (in32 + x) | 0;

		dst[low + k] = sat16(rshiftRound((out2 + out1) | 0, 11));
		dst[high + k] = sat16(rshiftRound((out2 - out1) | 0, 11));
	}
}

const BANDS = 4;
const NOISE_LEVEL_SMOOTH_COEF_Q16 = 1024;
const NOISE_LEVELS_BIAS = 50;
const NEGATIVE_OFFSET_Q5 = 128;
const SNR_FACTOR_Q16 = 45000;
// SILK_FIX_CONST(SPEECH_ACTIVITY_DTX_THRES, 8): below this, libopus marks a frame inactive.
const ACTIVITY_THRESHOLD_Q8 = 13;

/** @internal Constructor options for {@link Vad}. */
export interface VadProps {
	/** The rate the PCM arrives at, in Hz: one SILK encodes at directly without the analysis. */
	sampleRate: 8000 | 12000;

	/** The analysis block, which SILK takes as 10 or 20 ms. */
	block: 10 | 20;
}

/**
 * @internal SILK's voice activity detector: it splits the signal into four bands, tracks each
 * band's noise floor, and scores speech by how far the bands rise above it.
 *
 * The floor adapts fast for the first 20 seconds and slowly after, so a steady noise that starts
 * later, louder than the room, reads as speech until the floor catches up with it. libopus decides
 * DTX on it alone only at 8 and 12 kHz, where it runs no tonality analysis.
 *
 * Takes frames of any length and scores each complete block.
 */
export class Vad {
	readonly #length: number;
	readonly #block: 10 | 20;

	readonly #pending: Int16Array;
	#filled = 0;

	// The analysis state: the three filterbank stages, the low band's differentiator, and each band's
	// energy in the previous block's last subframe, which counts again as look-back.
	#ana = [new Int32Array(2), new Int32Array(2), new Int32Array(2)];
	#hp = 0;
	#subframe = new Int32Array(BANDS);

	// The noise floor, which survives a timeline break: the room is still the room.
	readonly #bias = new Int32Array(BANDS);
	readonly #nl = new Int32Array(BANDS);
	readonly #invNl = new Int32Array(BANDS);
	#counter = 15;

	constructor(props: VadProps) {
		this.#block = props.block;
		this.#length = (props.sampleRate / 1000) * props.block;
		this.#pending = new Int16Array(this.#length);

		for (let b = 0; b < BANDS; b++) {
			this.#bias[b] = Math.max(Math.trunc(NOISE_LEVELS_BIAS / (b + 1)), 1);
			this.#nl[b] = 100 * this.#bias[b];
			this.#invNl[b] = div32(INT32_MAX, this.#nl[b]);
		}
	}

	/**
	 * Score a frame, returning whether any block it completes holds speech, or undefined when it
	 * completes none. Stereo is scored on its mid channel, as SILK does.
	 */
	push(frame: AudioFrame): boolean | undefined {
		const input = mono(frame.channels);
		const pcm = new Int16Array(input.length);
		for (let i = 0; i < input.length; i++) pcm[i] = sat16(Math.round(input[i] * 32768));

		let result: boolean | undefined;
		for (let offset = 0; offset < pcm.length; ) {
			const take = Math.min(this.#length - this.#filled, pcm.length - offset);
			this.#pending.set(pcm.subarray(offset, offset + take), this.#filled);
			this.#filled += take;
			offset += take;
			if (this.#filled < this.#length) break;
			this.#filled = 0;
			const active = this.activity(this.#pending) >= ACTIVITY_THRESHOLD_Q8;
			result = (result ?? false) || active;
		}
		return result;
	}

	/** Forget the signal, as after a break in the timeline, but keep the learned noise floor. */
	reset(): void {
		this.#filled = 0;
		for (const state of this.#ana) state.fill(0);
		this.#hp = 0;
		this.#subframe.fill(0);
	}

	/** silk_VAD_GetSA_Q8: the speech activity of one block, in Q8. Exposed to compare against libopus. */
	activity(pIn: Int16Array): number {
		const frameLength = this.#length;
		const dec1 = frameLength >> 1;
		const dec2 = frameLength >> 2;
		const dec3 = frameLength >> 3;

		// The four bands, an octave each from the top, with scratch after the lowest:
		// [0-1/16 | scratch | 1/16-1/8 | 1/8-1/4 | 1/4-1/2 of the rate].
		const offsets = [0, dec3 + dec2, 0, 0];
		offsets[2] = offsets[1] + dec3;
		offsets[3] = offsets[2] + dec2;
		const x = new Int16Array(offsets[3] + dec1);

		// Split the whole band in halves, then the lower half twice more.
		anaFiltBank1(pIn, 0, this.#ana[0], x, 0, offsets[3], frameLength);
		anaFiltBank1(x, 0, this.#ana[1], x, 0, offsets[2], dec1);
		anaFiltBank1(x, 0, this.#ana[2], x, 0, offsets[1], dec2);

		// The lowest band is high-passed by a differentiator.
		x[dec3 - 1] = x[dec3 - 1] >> 1;
		const hpTmp = x[dec3 - 1];
		for (let i = dec3 - 1; i > 0; i--) {
			x[i - 1] = x[i - 1] >> 1;
			x[i] -= x[i - 1];
		}
		x[0] -= this.#hp;
		this.#hp = hpTmp;

		const xnrg = new Int32Array(BANDS);
		for (let b = 0; b < BANDS; b++) {
			const length = frameLength >> Math.min(BANDS - b, BANDS - 1);
			const subLength = length >> 2;
			let at = offsets[b];
			let sum = 0;
			xnrg[b] = this.#subframe[b];
			for (let s = 0; s < 4; s++) {
				sum = 0;
				for (let i = 0; i < subLength; i++) {
					const v = x[at + i] >> 3;
					sum = smulbb(v, v) + sum;
				}
				// The last subframe is look-ahead, so it counts half now and in full next block.
				xnrg[b] = addPosSat32(xnrg[b], s < 3 ? sum : sum >> 1);
				at += subLength;
			}
			this.#subframe[b] = sum;
		}

		this.#noiseLevels(xnrg);

		let sumSquared = 0;
		for (let b = 0; b < BANDS; b++) {
			const speech = xnrg[b] - this.#nl[b];
			if (speech <= 0) continue;
			const ratio =
				(xnrg[b] & 0xff800000) === 0
					? div32(xnrg[b] << 8, this.#nl[b] + 1)
					: div32(xnrg[b], (this.#nl[b] >> 8) + 1);
			const snr = lin2log(ratio) - 8 * 128;
			sumSquared = (sumSquared + smulbb(snr, snr)) | 0;
		}
		sumSquared = Math.trunc(sumSquared / BANDS);
		const snrDb = int16(3 * sqrtApprox(sumSquared));

		let sa = sigmQ15(smulwb(SNR_FACTOR_Q16, snrDb) - NEGATIVE_OFFSET_Q5);

		// Scale by power: weaker blocks need a clearer SNR, weighting the higher bands more.
		let speech = 0;
		for (let b = 0; b < BANDS; b++) speech = (speech + (b + 1) * ((xnrg[b] - this.#nl[b]) >> 4)) | 0;
		if (this.#block === 20) speech >>= 1;
		if (speech <= 0) {
			sa >>= 1;
		} else if (speech < 16384) {
			speech = sqrtApprox(speech << 16);
			sa = smulwb(32768 + speech, sa);
		}

		return Math.min(sa >> 7, 255);
	}

	// silk_VAD_GetNoiseLevels: smooth each band's floor toward its energy, slowly while it is loud.
	#noiseLevels(x: Int32Array): void {
		let minCoef = 0;
		if (this.#counter < 1000) {
			minCoef = Math.trunc(32767 / ((this.#counter >> 4) + 1));
			this.#counter++;
		}

		for (let k = 0; k < BANDS; k++) {
			let nl = this.#nl[k];
			const nrg = addPosSat32(x[k], this.#bias[k]);
			const invNrg = div32(INT32_MAX, nrg);

			let coef: number;
			if (nrg > nl << 3) coef = NOISE_LEVEL_SMOOTH_COEF_Q16 >> 3;
			else if (nrg < nl) coef = NOISE_LEVEL_SMOOTH_COEF_Q16;
			else coef = smulwb(smulww(invNrg, nl), NOISE_LEVEL_SMOOTH_COEF_Q16 << 1);
			coef = Math.max(coef, minCoef);

			this.#invNl[k] = smlawb(this.#invNl[k], (invNrg - this.#invNl[k]) | 0, coef);
			nl = div32(INT32_MAX, this.#invNl[k]);
			this.#nl[k] = Math.min(nl, 0x00ffffff);
		}
	}
}

// The mid channel, which SILK scores for stereo.
function mono(channels: Float32Array[]): Float32Array {
	if (channels.length === 1) return channels[0];
	const out = new Float32Array(channels[0].length);
	for (const channel of channels) {
		for (let i = 0; i < out.length; i++) out[i] += channel[i];
	}
	for (let i = 0; i < out.length; i++) out[i] /= channels.length;
	return out;
}
