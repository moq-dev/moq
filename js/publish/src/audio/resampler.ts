import { Time } from "@moq/net";
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

	// Input samples per output sample.
	readonly #ratio: number;

	// The previous chunk's last sample per channel. An output sample landing across the boundary
	// interpolates from this, which is what keeps the seam silent.
	#tail: Float32Array | undefined;

	// Running totals, which is what makes the conversion continuous rather than per-chunk.
	#inputIndex = 0;
	#outputIndex = 0;

	constructor(props: ResamplerProps) {
		if (props.from <= 0 || props.to <= 0) throw new Error("invalid sample rate");
		if (props.channels <= 0) throw new Error("invalid channel count");

		this.#from = props.from;
		this.#channels = props.channels;
		this.#ratio = props.from / props.to;
	}

	/** Convert one chunk, or undefined when it was too short to complete an output sample. */
	push(input: AudioFrame): AudioFrame | undefined {
		if (input.channels.length !== this.#channels) throw new Error("wrong number of channels");

		const samples = input.channels[0]?.length ?? 0;
		for (const channel of input.channels) {
			if (channel.length !== samples) throw new Error("mismatching number of samples");
		}
		if (samples === 0) return undefined;

		const base = this.#inputIndex;
		const last = base + samples - 1;

		// How many output samples this chunk can complete. Each needs the input sample after it to
		// interpolate toward, so we stop as soon as that one would fall past the end.
		let count = 0;
		while (Math.floor((this.#outputIndex + count) * this.#ratio) + 1 <= last) count++;

		const first = this.#outputIndex;
		this.#outputIndex += count;

		const channels = Array.from({ length: this.#channels }, () => new Float32Array(count));

		for (let n = 0; n < count; n++) {
			const position = (first + n) * this.#ratio;
			const index = Math.floor(position);
			const fraction = position - index;

			for (let channel = 0; channel < this.#channels; channel++) {
				// index is never below base - 1: the previous chunk stopped exactly when it ran out.
				const a = index < base ? (this.#tail?.[channel] ?? 0) : input.channels[channel][index - base];
				const b = input.channels[channel][index + 1 - base];
				channels[channel][n] = a + (b - a) * fraction;
			}
		}

		this.#carry(input.channels, samples);
		if (count === 0) return undefined;

		// Where the first output sample landed relative to this chunk's start, in input samples.
		// Slightly negative when it interpolates back across the boundary.
		const offset = first * this.#ratio - base;
		const timestamp = (input.timestamp + Time.Micro.fromSecond((offset / this.#from) as Time.Second)) as Time.Micro;

		return { timestamp, channels };
	}

	#carry(channels: Float32Array[], samples: number): void {
		this.#tail ??= new Float32Array(this.#channels);
		for (let channel = 0; channel < this.#channels; channel++) {
			this.#tail[channel] = channels[channel][samples - 1];
		}
		this.#inputIndex += samples;
	}
}
