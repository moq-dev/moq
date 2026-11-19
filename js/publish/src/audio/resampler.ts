import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";

/** Constructor options for {@link Resampler}. */
export interface ResamplerProps {
	/** The rate the samples arrive at, in Hz. */
	from: number;

	/** The rate to convert to, in Hz. */
	to: number;

	/** The channel count, which conversion preserves. */
	channels: number;
}

// Input samples each side of an output sample that its kernel reads, converting up or between
// equal rates. 24 taps of a Kaiser-windowed sinc hold every fractional position within 0.02 dB to
// 20 kHz at 48 kHz, so a ratio the drift loop steers colors nothing Opus keeps.
const HALF = 12;

// The Kaiser window's shape: about 60 dB of stopband, so images of a rate conversion stay inaudible.
const BETA = 6;

// Fractional positions the kernel is tabulated at, interpolated linearly between. Enough that the
// interpolation error sits below the window's own.
const PHASES = 256;

/**
 * Converts planar PCM to a different sample rate, carrying state across chunks.
 *
 * A capture graph runs at whatever rate we ask for, but a decoded file arrives at the rate it was
 * authored at, and Opus only encodes at a handful of rates (44.1kHz not among them). WebCodecs
 * rejects input whose rate doesn't match the encoder config rather than converting for us, so a
 * 44.1kHz file has to be converted here before it can be published as Opus. The same converter,
 * between equal rates, steers a capture's sample count onto the media clock by a few hundred ppm.
 *
 * A windowed sinc, tabulated by fractional position, cut off at the lower of the two rates' Nyquist
 * so converting down does not alias. Between equal rates at the nominal ratio it passes samples
 * through unchanged, only delayed by the kernel's half width.
 */
export class Resampler {
	readonly #from: number;
	readonly #channels: number;

	// Input samples per output sample at the nominal rates, and as last adjusted.
	readonly #nominal: number;
	#ratio: number;

	// Where the current ratio took over: the input position of the first output sample it
	// produced. An adjustment starts a new segment, so outputs before it keep their positions.
	#start = 0;

	// The kernel's taps each side of an output position, and its coefficients by tap for each of
	// `PHASES + 1` fractional positions, one row per position.
	readonly #half: number;
	readonly #table: Float32Array;
	// The kernel for the output sample being computed, interpolated between two rows.
	readonly #kernel: Float32Array;

	// Per channel, the input samples a kernel may still read from earlier chunks, followed by the
	// current chunk. Zeros before the first input, so the converter opens with the kernel's tail.
	#buffers: Float32Array[];

	// Running totals, which is what makes the conversion continuous rather than per-chunk: input
	// samples consumed, and output samples produced since the current ratio took over.
	#inputIndex = 0;
	#outputIndex = 0;

	constructor(props: ResamplerProps) {
		if (props.from <= 0 || props.to <= 0) throw new Error("invalid sample rate");
		if (props.channels <= 0) throw new Error("invalid channel count");

		this.#from = props.from;
		this.#channels = props.channels;
		this.#nominal = props.from / props.to;
		this.#ratio = this.#nominal;

		// Converting down, the cutoff moves to the output's Nyquist and the kernel widens to match.
		const cutoff = Math.min(1, props.to / props.from);
		const width = HALF / cutoff;
		this.#half = Math.ceil(width);
		const taps = 2 * this.#half;
		this.#table = new Float32Array((PHASES + 1) * taps);
		for (let phase = 0; phase <= PHASES; phase++) {
			const row = this.#table.subarray(phase * taps, (phase + 1) * taps);
			let sum = 0;
			for (let k = 0; k < taps; k++) {
				// The tap's distance from the output position, in input samples.
				const t = k - this.#half + 1 - phase / PHASES;
				const x = Math.PI * cutoff * t;
				const sinc = t === 0 ? 1 : Math.sin(x) / x;
				const u = t / width;
				row[k] = Math.abs(u) < 1 ? sinc * bessel(BETA * Math.sqrt(1 - u * u)) : 0;
				sum += row[k];
			}
			// Unity gain at DC in every row, so a steered ratio does not modulate the level.
			for (let k = 0; k < taps; k++) row[k] /= sum;
		}
		this.#kernel = new Float32Array(taps);
		this.#buffers = Array.from({ length: this.#channels }, () => new Float32Array(taps));
	}

	/**
	 * Produce `relative` times the nominal output per input from the next output sample on, such as
	 * 1.0001 to run 100 ppm ahead of the input's clock.
	 */
	adjust(relative: number): void {
		if (!(relative > 0)) throw new Error("invalid ratio");
		this.#start = this.#position(0);
		this.#outputIndex = 0;
		this.#ratio = this.#nominal / relative;
	}

	/** Output samples the input taken so far still owes, at the current ratio: the kernel's lookahead. */
	get pending(): number {
		return (this.#inputIndex - this.#position(0)) / this.#ratio;
	}

	// The input position of the `n`th output sample from the next.
	#position(n: number): number {
		return this.#start + (this.#outputIndex + n) * this.#ratio;
	}

	/** Convert one chunk, or undefined when it was too short to complete an output sample. */
	push(input: AudioFrame): AudioFrame | undefined {
		// Where the first output sample lands relative to this chunk's start, in input samples.
		// Negative while the kernel's lookahead holds samples from earlier chunks.
		const offset = this.#position(0) - this.#inputIndex;
		const channels = this.convert(input.channels);
		if (!channels) return undefined;

		// A signed offset, so the sum is taken in microseconds rather than with Duration arithmetic.
		const timestamp = Duration.micros(input.timestamp.asMicros() + (offset / this.#from) * 1_000_000);
		return { timestamp, channels };
	}

	/** Convert one chunk's samples, or undefined when it was too short to complete an output sample. */
	convert(input: Float32Array[]): Float32Array[] | undefined {
		if (input.length !== this.#channels) throw new Error("wrong number of channels");

		const samples = input[0]?.length ?? 0;
		for (const channel of input) {
			if (channel.length !== samples) throw new Error("mismatching number of samples");
		}
		if (samples === 0) return undefined;

		const half = this.#half;
		const taps = 2 * half;
		const table = this.#table;
		const kernel = this.#kernel;

		// The buffer holds `taps` samples from before this chunk, so index `i` of the input timeline
		// sits at `i - origin` in it.
		const origin = this.#inputIndex - taps;
		const last = this.#inputIndex + samples - 1;
		if (this.#buffers[0].length < taps + samples) {
			this.#buffers = this.#buffers.map((old) => {
				const grown = new Float32Array(taps + samples);
				grown.set(old.subarray(0, taps));
				return grown;
			});
		}
		for (let channel = 0; channel < this.#channels; channel++) {
			this.#buffers[channel].set(input[channel], taps);
		}

		// How many output samples this chunk can complete. Each reads `half` input samples past its
		// position, so we stop as soon as the last of them would fall past the end.
		let count = 0;
		while (Math.floor(this.#position(count)) + half <= last) count++;

		const channels = Array.from({ length: this.#channels }, () => new Float32Array(count));

		for (let n = 0; n < count; n++) {
			const position = this.#position(n);
			const index = Math.floor(position);
			const phase = (position - index) * PHASES;
			const row = Math.floor(phase);
			const weight = phase - row;
			const a = row * taps;
			const b = a + taps;
			for (let k = 0; k < taps; k++) kernel[k] = table[a + k] + (table[b + k] - table[a + k]) * weight;

			// The first tap reads `half - 1` samples before the position's floor, which the previous
			// chunk kept: it stopped as soon as an output needed past its end.
			const first = index - half + 1 - origin;
			for (let channel = 0; channel < this.#channels; channel++) {
				const buffer = this.#buffers[channel];
				let sum = 0;
				for (let k = 0; k < taps; k++) sum += kernel[k] * buffer[first + k];
				channels[channel][n] = sum;
			}
		}

		this.#outputIndex += count;
		this.#inputIndex += samples;
		// Keep the chunk's last `taps` samples, the most the next chunk's kernels read back.
		for (const buffer of this.#buffers) buffer.copyWithin(0, samples, samples + taps);
		return count === 0 ? undefined : channels;
	}
}

// The zeroth-order modified Bessel function of the first kind, which shapes the Kaiser window.
function bessel(x: number): number {
	let sum = 1;
	let term = 1;
	for (let k = 1; term > 1e-12 * sum; k++) {
		term *= (x / (2 * k)) ** 2;
		sum += term;
	}
	return sum;
}
