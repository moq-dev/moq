/*
 * A port of libopus's tonality analysis (src/analysis.c), as far as its activity probability needs,
 * and of the activity decision its encoder makes from it for DTX (src/opus_encoder.c), from libopus
 * 1.6.1.
 *
 * Copyright (c) 2011 Xiph.Org Foundation, written by Jean-Marc Valin.
 * Redistribution and use in source and binary forms, with or without modification, are permitted
 * provided that the following conditions are met:
 * - Redistributions of source code must retain the above copyright notice, this list of
 *   conditions and the following disclaimer.
 * - Redistributions in binary form must reproduce the above copyright notice, this list of
 *   conditions and the following disclaimer in the documentation and/or other materials provided
 *   with the distribution.
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
 * FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE FOUNDATION OR
 * CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
 * IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
 * OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

import type { AudioFrame } from "./capture";
import { Mlp } from "./mlp";
import { Resampler } from "./resampler";

// The analysis runs at 24 kHz on 20 ms windows, advancing 20 ms at a time over a 30 ms buffer.
const N = 480;
const N2 = 240;
const BUF = 720;
const NB_FRAMES = 8;
const NB_TBANDS = 18;
const NB_TONAL_SKIP_BANDS = 9;
const DETECT_SIZE = 100;
const COUNT_MAX = 10_000;

// Below this activity probability a frame is noise, unless it is loud (DTX_ACTIVITY_THRESHOLD).
const ACTIVITY_THRESHOLD = 0.1;
// Noise within 25 dB of the loudest recent activity still counts as activity.
const PSEUDO_SNR_THRESHOLD = 316.23;
// What libopus takes as digital silence from float input (lsb_depth 24).
const SILENCE = 1 / (1 << 24);

const TBANDS = [4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 136, 160, 192, 240];
const STD_FEATURE_BIAS = [5.684947, 3.475288, 1.770634, 1.599784, 3.773215, 2.163313, 1.260756, 1.116868, 1.918795];

// libopus prints its tables to six decimals, so they are rebuilt the same way.
const decimal6 = (x: number) => Math.fround(Math.round(x * 1e6) / 1e6);

// sin^2 rising over half the window, mirrored for the other half.
const WINDOW = Float32Array.from({ length: N2 }, (_, i) => decimal6(Math.sin((Math.PI * (i + 1)) / N) ** 2));

// An orthonormal DCT-II of the 16 band energies.
const DCT = Float32Array.from({ length: 128 }, (_, n) => {
	const i = n >> 4;
	const b = n & 15;
	const scale = i === 0 ? Math.sqrt(1 / 16) : Math.sqrt(2 / 16);
	return decimal6(scale * Math.cos((Math.PI * (b + 0.5) * i) / 16));
});

// fast_atan2f: a rational atan2 approximation, 0 for vanishing inputs.
function fastAtan2(y: number, x: number): number {
	const A = 0.43157974;
	const B = 0.67848403;
	const C = 0.08595542;
	const E = Math.PI / 2;
	const x2 = x * x;
	const y2 = y * y;
	if (x2 + y2 < 1e-18) return 0;
	if (x2 < y2) {
		const den = (y2 + B * x2) * (y2 + C * x2);
		return (-x * y * (y2 + A * x2)) / den + (y < 0 ? -E : E);
	}
	const den = (x2 + B * y2) * (x2 + C * y2);
	return (x * y * (x2 + A * y2)) / den + (y < 0 ? -E : E) - (x * y < 0 ? -E : E);
}

// lrintf: round to the nearest integer, ties to even.
function rint(x: number): number {
	const r = Math.round(x);
	return r - x === 0.5 && r % 2 !== 0 ? r - 1 : r;
}

// A mixed-radix complex FFT of 480 points, scaled by 1/480 as CELT's is.
const FFT_FACTORS = [4, 4, 2, 3, 5];

// One decimation-in-time stage: blocks of p*m outputs, each combining p transforms of length m.
interface FftStage {
	p: number;
	m: number;
	// The twiddle for subsequence q at bin k, at q * m + k.
	twR: Float64Array;
	twI: Float64Array;
	// The p-point DFT's roots, at r * p + q.
	rootR: Float64Array;
	rootI: Float64Array;
}

class Fft {
	// Which input lands in each output slot before the first stage, as the recursion would read it.
	readonly #order = new Int32Array(N);
	// Innermost first.
	readonly #stages: FftStage[] = [];
	readonly #tr = new Float64Array(5);
	readonly #ti = new Float64Array(5);

	constructor() {
		const order = (at: number, stride: number, n: number, out: number, depth: number): void => {
			if (n === 1) {
				this.#order[out] = at;
				return;
			}
			const p = FFT_FACTORS[depth];
			for (let q = 0; q < p; q++) order(at + q * stride, stride * p, n / p, out + (q * n) / p, depth + 1);
		};
		order(0, 1, N, 0, 0);

		let stride = N;
		for (const p of [...FFT_FACTORS].reverse()) {
			stride /= p;
			const m = N / stride / p;
			const twR = new Float64Array(p * m);
			const twI = new Float64Array(p * m);
			for (let q = 0; q < p; q++) {
				for (let k = 0; k < m; k++) {
					const angle = (-2 * Math.PI * q * k * stride) / N;
					twR[q * m + k] = Math.cos(angle);
					twI[q * m + k] = Math.sin(angle);
				}
			}
			const rootR = new Float64Array(p * p);
			const rootI = new Float64Array(p * p);
			for (let i = 0; i < p * p; i++) {
				const angle = (-2 * Math.PI * (Math.floor(i / p) * (i % p))) / p;
				rootR[i] = Math.cos(angle);
				rootI[i] = Math.sin(angle);
			}
			this.#stages.push({ p, m, twR, twI, rootR, rootI });
		}
	}

	run(inR: Float64Array, inI: Float64Array, outR: Float64Array, outI: Float64Array): void {
		for (let i = 0; i < N; i++) {
			outR[i] = inR[this.#order[i]] / N;
			outI[i] = inI[this.#order[i]] / N;
		}
		const tr = this.#tr;
		const ti = this.#ti;
		for (const { p, m, twR, twI, rootR, rootI } of this.#stages) {
			for (let out = 0; out < N; out += p * m) {
				for (let k = 0; k < m; k++) {
					for (let q = 0; q < p; q++) {
						const x = out + q * m + k;
						const w = q * m + k;
						tr[q] = outR[x] * twR[w] - outI[x] * twI[w];
						ti[q] = outR[x] * twI[w] + outI[x] * twR[w];
					}
					for (let r = 0; r < p; r++) {
						let sr = 0;
						let si = 0;
						for (let q = 0; q < p; q++) {
							const w = r * p + q;
							sr += tr[q] * rootR[w] - ti[q] * rootI[w];
							si += tr[q] * rootI[w] + ti[q] * rootR[w];
						}
						outR[out + r * m + k] = sr;
						outI[out + r * m + k] = si;
					}
				}
			}
		}
	}
}

// What one analysis window left for the encoder to read.
interface Info {
	valid: boolean;
	probability: number;
}

/**
 * @internal libopus's tonality analysis, reduced to the activity probability its encoder gates DTX
 * on at complexity 7 and up: the spectrum's tonality, noisiness, stationarity, and band energies
 * against their learned range feed a small recurrent network that scores speech or music against
 * noise. Unlike SILK's noise floor, it learns a noise that starts mid-call within a few frames.
 *
 * Takes codec frames at 16, 24, or 48 kHz as libopus does, and any other rate through a linear
 * conversion to 48 kHz first.
 */
export class Analysis {
	readonly #sampleRate: number;
	readonly #resampler?: Resampler;
	// Samples per output sample at 24 kHz after any conversion: 2 at 48 kHz, 1 at 24 kHz, 2/3 at 16 kHz.
	readonly #down: 1 | 2 | 3;
	// silk_resampler_down2_hp's two allpass states, and an odd sample waiting for its pair.
	readonly #down2 = new Float64Array(2);
	#odd: number | undefined;

	readonly #inmem = new Float32Array(BUF);
	#memFill = N2;

	readonly #angle = new Float32Array(N2);
	readonly #dAngle = new Float32Array(N2);
	readonly #d2Angle = new Float32Array(N2);
	readonly #prevBandTonality = new Float32Array(NB_TBANDS);
	#prevTonality = 0;
	readonly #E = Array.from({ length: NB_FRAMES }, () => new Float32Array(NB_TBANDS));
	readonly #logE = Array.from({ length: NB_FRAMES }, () => new Float32Array(NB_TBANDS));
	readonly #lowE = new Float32Array(NB_TBANDS);
	readonly #highE = new Float32Array(NB_TBANDS);
	readonly #mem = new Float32Array(32);
	readonly #cmean = new Float32Array(8);
	readonly #std = new Float32Array(9);
	#etracker = 0;
	#lowECount = 0;
	#eCount = 0;
	#count = 0;
	readonly #mlp = new Mlp();

	// The ring the encoder reads each frame's analysis from, as tonality_get_info does.
	readonly #info: Info[] = Array.from({ length: DETECT_SIZE }, () => ({ valid: false, probability: 0 }));
	#writePos = 0;
	#readPos = 0;
	// How far the reader is into its window, in input samples times 50: a window is the input rate.
	#readOffset = 0;

	// The encoder's peak_signal_energy: the loudest recent active frame, decaying 0.1% a frame.
	#peak = 0;

	// Scratch for one window.
	readonly #inR = new Float64Array(N);
	readonly #inI = new Float64Array(N);
	readonly #outR = new Float64Array(N);
	readonly #outI = new Float64Array(N);
	readonly #fft = new Fft();
	readonly #tonality = new Float32Array(N2);
	readonly #tonality2 = new Float32Array(N2);
	readonly #noisiness = new Float32Array(N2);
	readonly #features = new Float32Array(25);

	constructor(props: { sampleRate: number }) {
		this.#sampleRate = props.sampleRate;
		if (props.sampleRate === 24_000) this.#down = 1;
		else if (props.sampleRate === 16_000) this.#down = 3;
		else {
			this.#down = 2;
			if (props.sampleRate !== 48_000)
				this.#resampler = new Resampler({ from: props.sampleRate, to: 48_000, channels: 1 });
		}
	}

	/**
	 * Whether libopus would count a codec frame as active for DTX: not digital silence, and either
	 * scored as speech or music, or loud against the peak. A frame without a valid analysis counts
	 * as active, which only the first frame under 20 ms can be.
	 */
	push(frame: AudioFrame): boolean {
		const probability = this.probability(frame);
		const samples = frame.channels[0].length;
		let max = 0;
		let energy = 0;
		for (const channel of frame.channels) {
			for (let i = 0; i < samples; i++) {
				const x = channel[i];
				max = Math.max(max, Math.abs(x));
				energy += x * x;
			}
		}
		if (max <= SILENCE) return false;

		// libopus raises the peak on a frame without a valid analysis too.
		energy /= samples * frame.channels.length;
		if (probability === undefined || probability > ACTIVITY_THRESHOLD)
			this.#peak = Math.max(0.999 * this.#peak, energy);
		if (probability === undefined) return true;
		return probability >= ACTIVITY_THRESHOLD || this.#peak < PSEUDO_SNR_THRESHOLD * energy;
	}

	/**
	 * run_analysis and tonality_get_info: analyze a codec frame and return the activity probability
	 * the encoder reads for it, or undefined when it has none. Exposed to compare against libopus.
	 */
	probability(frame: AudioFrame): number | undefined {
		let input = mono(frame.channels);
		const samples = input.length;
		if (this.#resampler) {
			const resampled = this.#resampler.push({ timestamp: frame.timestamp, channels: [input] });
			input = resampled ? resampled.channels[0] : new Float32Array(0);
		}
		this.#append(input);

		// Where the frame reads from: libopus advances one window per 20 ms of frames, which this
		// counts exactly, so a frame that is no whole number of 2.5 ms subframes (AAC's 1024 samples)
		// does not drift.
		let pos = this.#readPos;
		this.#readOffset += samples * 50;
		while (this.#readOffset >= this.#sampleRate) {
			this.#readOffset -= this.#sampleRate;
			this.#readPos = (this.#readPos + 1) % DETECT_SIZE;
		}
		// A long frame reads the second window rather than the first.
		if (samples * 50 > this.#sampleRate && pos !== this.#writePos) pos = (pos + 1) % DETECT_SIZE;
		if (pos === this.#writePos) pos--;
		if (pos < 0) pos = DETECT_SIZE - 1;

		const info = this.#info[pos];
		return info.valid ? info.probability : undefined;
	}

	// Downmixed to 24 kHz on CELT's int16 scale, filling the buffer and analyzing each full window.
	#append(input: Float32Array): void {
		const scaled = (x: number) => x * 32768;
		if (this.#down === 1) {
			for (let i = 0; i < input.length; i++) this.#sample(Math.fround(scaled(input[i])));
			return;
		}
		// 16 kHz repeats each sample three times before halving, aliasing and all.
		const repeat = this.#down === 3 ? 3 : 1;
		for (let i = 0; i < input.length; i++) {
			const x = Math.fround(scaled(input[i]));
			for (let r = 0; r < repeat; r++) {
				if (this.#odd === undefined) {
					this.#odd = x;
					continue;
				}
				this.#sample(this.#halve(this.#odd, x));
				this.#odd = undefined;
			}
		}
	}

	// silk_resampler_down2_hp's low band: two allpass sections, one per phase, averaged.
	#halve(even: number, odd: number): number {
		const S = this.#down2;
		let Y = even - S[0];
		let X = Math.fround(0.6074371 * Y);
		let out = Math.fround(S[0] + X);
		S[0] = Math.fround(even + X);

		Y = odd - S[1];
		X = Math.fround(0.15063 * Y);
		out = Math.fround(Math.fround(out + S[1]) + X);
		S[1] = Math.fround(odd + X);
		return Math.fround(0.5 * out);
	}

	#sample(x: number): void {
		this.#inmem[this.#memFill++] = x;
		if (this.#memFill < BUF) return;
		this.#window();
		this.#inmem.copyWithin(0, BUF - N2, BUF);
		this.#memFill = N2;
	}

	// tonality_analysis on the full buffer.
	#window(): void {
		const info = this.#info[this.#writePos];
		const prev = this.#info[(this.#writePos + DETECT_SIZE - 1) % DETECT_SIZE];
		this.#writePos = (this.#writePos + 1) % DETECT_SIZE;

		const inmem = this.#inmem;
		let max = 0;
		for (let i = 0; i < BUF; i++) max = Math.max(max, Math.abs(inmem[i]));
		// Digital silence repeats the previous window's analysis.
		if (max <= SILENCE) {
			info.valid = prev.valid;
			info.probability = prev.probability;
			return;
		}

		const alpha = 1 / Math.min(10, 1 + this.#count);
		const alphaE = 1 / Math.min(25, 1 + this.#count);

		// Two real windows, 10 ms apart, packed as one complex transform.
		const inR = this.#inR;
		const inI = this.#inI;
		for (let i = 0; i < N2; i++) {
			const w = WINDOW[i];
			inR[i] = Math.fround(w * inmem[i]);
			inI[i] = Math.fround(w * inmem[N2 + i]);
			inR[N - i - 1] = Math.fround(w * inmem[N - i - 1]);
			inI[N - i - 1] = Math.fround(w * inmem[N + N2 - i - 1]);
		}
		const outR = this.#outR;
		const outI = this.#outI;
		this.#fft.run(inR, inI, outR, outI);
		if (Number.isNaN(outR[0])) {
			info.valid = false;
			return;
		}

		// How steadily each bin's phase advances between the two windows and the previous call.
		const A = this.#angle;
		const dA = this.#dAngle;
		const d2A = this.#d2Angle;
		const tonality = this.#tonality;
		const tonality2 = this.#tonality2;
		const noisiness = this.#noisiness;
		const pi4 = Math.PI ** 4;
		for (let i = 1; i < N2; i++) {
			const X1r = outR[i] + outR[N - i];
			const X1i = outI[i] - outI[N - i];
			const X2r = outI[i] + outI[N - i];
			const X2i = outR[N - i] - outR[i];

			const angle = (0.5 / Math.PI) * fastAtan2(X1i, X1r);
			const dAngle = angle - A[i];
			const d2Angle = dAngle - dA[i];

			const angle2 = (0.5 / Math.PI) * fastAtan2(X2i, X2r);
			const dAngle2 = angle2 - angle;
			const d2Angle2 = dAngle2 - dAngle;

			let mod1 = d2Angle - rint(d2Angle);
			noisiness[i] = Math.abs(mod1);
			mod1 *= mod1;
			mod1 *= mod1;

			let mod2 = d2Angle2 - rint(d2Angle2);
			noisiness[i] += Math.abs(mod2);
			mod2 *= mod2;
			mod2 *= mod2;

			const avgMod = 0.25 * (d2A[i] + mod1 + 2 * mod2);
			tonality[i] = 1 / (1 + 40 * 16 * pi4 * avgMod) - 0.015;
			tonality2[i] = 1 / (1 + 40 * 16 * pi4 * mod2) - 0.015;

			A[i] = angle2;
			dA[i] = dAngle2;
			d2A[i] = mod2;
		}
		for (let i = 2; i < N2 - 1; i++) {
			const tt = Math.min(tonality2[i], Math.max(tonality2[i - 1], tonality2[i + 1]));
			tonality[i] = 0.9 * Math.max(tonality[i], tt - 0.1);
		}

		let frameTonality = 0;
		let maxFrameTonality = 0;
		let frameNoisiness = 0;
		let frameStationarity = 0;
		let relativeE = 0;
		let frameLoudness = 0;
		let slope = 0;
		const lowE = this.#lowE;
		const highE = this.#highE;
		if (this.#count === 0) {
			lowE.fill(1e10);
			highE.fill(-1e10);
		}
		const E = this.#E[this.#eCount];
		const logE = this.#logE[this.#eCount];
		const bandTonality = new Float32Array(NB_TBANDS);
		for (let b = 0; b < NB_TBANDS; b++) {
			let bandE = 0;
			let tE = 0;
			let nE = 0;
			for (let i = TBANDS[b]; i < TBANDS[b + 1]; i++) {
				let binE =
					outR[i] * outR[i] + outR[N - i] * outR[N - i] + outI[i] * outI[i] + outI[N - i] * outI[N - i];
				binE /= 32768 * 32768;
				bandE += binE;
				tE += binE * Math.max(0, tonality[i]);
				nE += binE * 2 * (0.5 - noisiness[i]);
			}
			// Extreme band energies would turn into NaN later.
			if (!(bandE < 1e9)) {
				info.valid = false;
				return;
			}

			E[b] = bandE;
			frameNoisiness += nE / (1e-15 + bandE);
			frameLoudness += Math.sqrt(bandE + 1e-10);
			logE[b] = Math.log(bandE + 1e-10);
			if (this.#count === 0) highE[b] = lowE[b] = logE[b];
			if (highE[b] > lowE[b] + 7.5) {
				if (highE[b] - logE[b] > logE[b] - lowE[b]) highE[b] -= 0.01;
				else lowE[b] += 0.01;
			}
			if (logE[b] > highE[b]) {
				highE[b] = logE[b];
				lowE[b] = Math.max(highE[b] - 15, lowE[b]);
			} else if (logE[b] < lowE[b]) {
				lowE[b] = logE[b];
				highE[b] = Math.min(lowE[b] + 15, highE[b]);
			}
			relativeE += (logE[b] - lowE[b]) / (1e-5 + (highE[b] - lowE[b]));

			let L1 = 0;
			let L2 = 0;
			for (let i = 0; i < NB_FRAMES; i++) {
				L1 += Math.sqrt(this.#E[i][b]);
				L2 += this.#E[i][b];
			}
			let stationarity = Math.min(0.99, L1 / Math.sqrt(1e-15 + NB_FRAMES * L2));
			stationarity *= stationarity;
			stationarity *= stationarity;
			frameStationarity += stationarity;
			bandTonality[b] = Math.max(tE / (1e-15 + bandE), stationarity * this.#prevBandTonality[b]);
			frameTonality += bandTonality[b];
			if (b >= NB_TBANDS - NB_TONAL_SKIP_BANDS)
				frameTonality -= bandTonality[b - NB_TBANDS + NB_TONAL_SKIP_BANDS];
			maxFrameTonality = Math.max(maxFrameTonality, (1 + 0.03 * (b - NB_TBANDS)) * frameTonality);
			slope += bandTonality[b] * (b - 8);
			this.#prevBandTonality[b] = bandTonality[b];
		}

		// How far each frame's spectrum is from its nearest neighbour among the last eight.
		let specVariability = 0;
		for (let i = 0; i < NB_FRAMES; i++) {
			let minDist = 1e15;
			for (let j = 0; j < NB_FRAMES; j++) {
				if (j === i) continue;
				let dist = 0;
				for (let k = 0; k < NB_TBANDS; k++) {
					const d = this.#logE[i][k] - this.#logE[j][k];
					dist += d * d;
				}
				minDist = Math.min(minDist, dist);
			}
			specVariability += minDist;
		}
		specVariability = Math.sqrt(specVariability / NB_FRAMES / NB_TBANDS);

		frameLoudness = 20 * Math.log10(frameLoudness);
		this.#etracker = Math.max(this.#etracker - 0.003, frameLoudness);
		this.#lowECount *= 1 - alphaE;
		if (frameLoudness < this.#etracker - 30) this.#lowECount += alphaE;

		const BFCC = new Float32Array(8);
		const midE = new Float32Array(8);
		for (let i = 0; i < 8; i++) {
			let sum = 0;
			let mid = 0;
			for (let b = 0; b < 16; b++) {
				sum += DCT[i * 16 + b] * logE[b];
				mid += DCT[i * 16 + b] * 0.5 * (highE[b] + lowE[b]);
			}
			BFCC[i] = sum;
			midE[i] = mid;
		}

		frameStationarity /= NB_TBANDS;
		relativeE = this.#count < 10 ? 0.5 : relativeE / NB_TBANDS;
		frameNoisiness /= NB_TBANDS;
		const activity = frameNoisiness + (1 - frameNoisiness) * relativeE;
		frameTonality = Math.max(maxFrameTonality / (NB_TBANDS - NB_TONAL_SKIP_BANDS), this.#prevTonality * 0.8);
		this.#prevTonality = frameTonality;
		slope /= 8 * 8;

		this.#eCount = (this.#eCount + 1) % NB_FRAMES;
		this.#count = Math.min(this.#count + 1, COUNT_MAX);

		// The cepstrum's first differences and its deviation, against the last three frames.
		const f = this.#features;
		const mem = this.#mem;
		const cmean = this.#cmean;
		const std = this.#std;
		for (let i = 0; i < 4; i++)
			f[i] =
				-0.12299 * (BFCC[i] + mem[i + 24]) +
				0.49195 * (mem[i] + mem[i + 16]) +
				0.69693 * mem[i + 8] -
				1.4349 * cmean[i];
		for (let i = 0; i < 4; i++) cmean[i] = (1 - alpha) * cmean[i] + alpha * BFCC[i];
		for (let i = 0; i < 4; i++) f[4 + i] = 0.63246 * (BFCC[i] - mem[i + 24]) + 0.31623 * (mem[i] - mem[i + 16]);
		for (let i = 0; i < 3; i++)
			f[8 + i] = 0.53452 * (BFCC[i] + mem[i + 24]) - 0.26726 * (mem[i] + mem[i + 16]) - 0.53452 * mem[i + 8];
		if (this.#count > 5) for (let i = 0; i < 9; i++) std[i] = (1 - alpha) * std[i] + alpha * f[i] * f[i];
		for (let i = 0; i < 4; i++) f[i] = BFCC[i] - midE[i];
		for (let i = 0; i < 8; i++) {
			mem[i + 24] = mem[i + 16];
			mem[i + 16] = mem[i + 8];
			mem[i + 8] = mem[i];
			mem[i] = BFCC[i];
		}
		for (let i = 0; i < 9; i++) f[11 + i] = Math.sqrt(std[i]) - STD_FEATURE_BIAS[i];
		f[18] = specVariability - 0.78;
		f[20] = frameTonality - 0.154723;
		f[21] = activity - 0.724643;
		f[22] = frameStationarity - 0.743717;
		f[23] = slope + 0.069216;
		f[24] = this.#lowECount - 0.06793;

		info.probability = this.#mlp.push(f);
		info.valid = true;
	}
}

// The mean of the channels, as libopus's analysis downmixes stereo.
function mono(channels: Float32Array[]): Float32Array {
	if (channels.length === 1) return channels[0];
	const out = new Float32Array(channels[0].length);
	for (const channel of channels) {
		for (let i = 0; i < out.length; i++) out[i] += channel[i];
	}
	for (let i = 0; i < out.length; i++) out[i] /= channels.length;
	return out;
}
