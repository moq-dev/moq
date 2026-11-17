/*
 * The network of libopus's tonality analysis (src/mlp.c, src/mlp_data.c) from libopus 1.6.1, whose
 * weights are unchanged since 1.3.
 *
 * Copyright (c) 2008-2011 Octasic Inc., 2012-2017 Jean-Marc Valin.
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

// mlp_data.c's int8 tables, in order: layer0 weights and bias, layer1 input weights, recurrent
// weights, and bias, then layer2 weights and bias.
const WEIGHTS = Int8Array.from(
	atob(
		[
			"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",
			"Ln/vfXXXi6WBvP+nsCBqBw51",
		].join(""),
	),
	(c) => c.charCodeAt(0),
);

// Each layer's view of WEIGHTS, as mlp_data.c lays them out.
let at = 0;
const take = (n: number) => {
	const view = WEIGHTS.subarray(at, at + n);
	at += n;
	return view;
};

const INPUTS = 25;
const HIDDEN = 32;
const STATE = 24;

const L0_WEIGHTS = take(INPUTS * HIDDEN);
const L0_BIAS = take(HIDDEN);
const L1_WEIGHTS = take(HIDDEN * STATE * 3);
const L1_RECURRENT = take(STATE * STATE * 3);
const L1_BIAS = take(STATE * 3);
const L2_WEIGHTS = take(STATE * 2);
const L2_BIAS = take(2);

const WEIGHTS_SCALE = 1 / 128;

// A rational approximation of tanh, clamped to [-1, 1].
function tansig(x: number): number {
	const x2 = x * x;
	const num = ((0.60863042 * x2 + 96.39235687) * x2 + 952.52801514) * x;
	const den = (11.88600922 * x2 + 413.36801147) * x2 + 952.72399902;
	return Math.max(-1, Math.min(1, num / den));
}

const sigmoid = (x: number) => 0.5 + 0.5 * tansig(0.5 * x);

// out[i] += sum_j weights[offset + j * stride + i] * x[j], for i < rows.
function gemm(
	out: Float64Array,
	weights: Int8Array,
	offset: number,
	rows: number,
	stride: number,
	x: ArrayLike<number>,
): void {
	for (let i = 0; i < rows; i++) {
		let sum = out[i];
		for (let j = 0; j < x.length; j++) sum += weights[offset + j * stride + i] * x[j];
		out[i] = sum;
	}
}

/**
 * @internal The analysis's recurrent network: a dense layer, a GRU, and a dense layer that scores
 * a frame's 25 features as music and as activity (speech or music, not noise).
 */
export class Mlp {
	readonly #state = new Float64Array(STATE);
	readonly #hidden = new Float64Array(HIDDEN);
	readonly #z = new Float64Array(STATE);
	readonly #r = new Float64Array(STATE);
	readonly #h = new Float64Array(STATE);
	readonly #gated = new Float64Array(STATE);
	readonly #out = new Float64Array(2);

	/** Run one frame's features, returning the activity probability. */
	push(features: ArrayLike<number>): number {
		const hidden = this.#hidden;
		hidden.set(L0_BIAS);
		gemm(hidden, L0_WEIGHTS, 0, HIDDEN, HIDDEN, features);
		for (let i = 0; i < HIDDEN; i++) hidden[i] = tansig(hidden[i] * WEIGHTS_SCALE);

		const state = this.#state;
		const z = this.#z;
		const r = this.#r;
		const h = this.#h;
		const stride = 3 * STATE;

		// The update gate, then the reset gate.
		z.set(L1_BIAS.subarray(0, STATE));
		gemm(z, L1_WEIGHTS, 0, STATE, stride, hidden);
		gemm(z, L1_RECURRENT, 0, STATE, stride, state);
		for (let i = 0; i < STATE; i++) z[i] = sigmoid(WEIGHTS_SCALE * z[i]);

		r.set(L1_BIAS.subarray(STATE, 2 * STATE));
		gemm(r, L1_WEIGHTS, STATE, STATE, stride, hidden);
		gemm(r, L1_RECURRENT, STATE, STATE, stride, state);
		for (let i = 0; i < STATE; i++) r[i] = sigmoid(WEIGHTS_SCALE * r[i]);

		h.set(L1_BIAS.subarray(2 * STATE));
		for (let i = 0; i < STATE; i++) this.#gated[i] = state[i] * r[i];
		gemm(h, L1_WEIGHTS, 2 * STATE, STATE, stride, hidden);
		gemm(h, L1_RECURRENT, 2 * STATE, STATE, stride, this.#gated);
		for (let i = 0; i < STATE; i++) state[i] = z[i] * state[i] + (1 - z[i]) * tansig(WEIGHTS_SCALE * h[i]);

		// The second output is activity; the first, music, is unused.
		const out = this.#out;
		out.set(L2_BIAS);
		gemm(out, L2_WEIGHTS, 0, 2, 2, state);
		return sigmoid(out[1] * WEIGHTS_SCALE);
	}
}
