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

/**
 * Converts planar PCM to a different sample rate, carrying state across chunks.
 *
 * A capture graph runs at whatever rate we ask for, but a decoded file arrives at the rate it was
 * authored at, and Opus only encodes at a handful of rates (44.1kHz not among them). WebCodecs
 * rejects input whose rate doesn't match the encoder config rather than converting for us, so a
 * 44.1kHz file has to be converted here before it can be published as Opus.
 *
 * Linear interpolation, with the previous chunk's last sample carried over so chunk boundaries
 * aren't audible. That's a step below a windowed-sinc filter, but its artifacts sit near the top of
 * the spectrum, which is the first thing Opus discards at publishing bitrates anyway.
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

	// The previous chunk's last sample per channel. An output sample landing across the boundary
	// interpolates from this, which is what keeps the seam silent.
	#tail: Float32Array | undefined;

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

	// The input position of the `n`th output sample from the next.
	#position(n: number): number {
		return this.#start + (this.#outputIndex + n) * this.#ratio;
	}

	/** Convert one chunk, or undefined when it was too short to complete an output sample. */
	push(input: AudioFrame): AudioFrame | undefined {
		// Where the first output sample lands relative to this chunk's start, in input samples.
		// Slightly negative when it interpolates back across the boundary.
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

		const base = this.#inputIndex;
		const last = base + samples - 1;

		// How many output samples this chunk can complete. Each needs the input sample after it to
		// interpolate toward, so we stop as soon as that one would fall past the end.
		let count = 0;
		while (Math.floor(this.#position(count)) + 1 <= last) count++;

		const channels = Array.from({ length: this.#channels }, () => new Float32Array(count));

		for (let n = 0; n < count; n++) {
			const position = this.#position(n);
			const index = Math.floor(position);
			const fraction = position - index;

			for (let channel = 0; channel < this.#channels; channel++) {
				// index is never below base - 1: the previous chunk stopped exactly when it ran out.
				const a = index < base ? (this.#tail?.[channel] ?? 0) : input[channel][index - base];
				const b = input[channel][index + 1 - base];
				channels[channel][n] = a + (b - a) * fraction;
			}
		}

		this.#outputIndex += count;
		this.#carry(input, samples);
		return count === 0 ? undefined : channels;
	}

	#carry(channels: Float32Array[], samples: number): void {
		this.#tail ??= new Float32Array(this.#channels);
		for (let channel = 0; channel < this.#channels; channel++) {
			this.#tail[channel] = channels[channel][samples - 1];
		}
		this.#inputIndex += samples;
	}
}
