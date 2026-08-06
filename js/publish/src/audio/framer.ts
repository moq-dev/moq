import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";

type FrameSize = { samples: number } | { duration: Time.Micro };

// How far an input timestamp may sit from where the running timeline expects it before it counts as
// a new segment rather than rounding noise. Capture quanta and container packets are both exact to
// well under this, so anything past it is a real gap.
const DISCONTINUITY = Time.Micro(1_000);

/** Collects capture quanta into codec-aligned audio frames. */
export class Framer {
	readonly #sampleRate: number;
	readonly #channels: number;
	readonly #size: FrameSize;

	#origin: Time.Micro | undefined;
	#frameIndex = 0;
	#sampleIndex = 0;
	#inputSamples = 0;
	#buffer: Float32Array[] = [];
	#written = 0;

	/** Create a framer for either a fixed sample count or a fixed presentation duration. */
	constructor(props: { sampleRate: number; channels: number; size: FrameSize }) {
		if (props.sampleRate <= 0) throw new Error("invalid sample rate");
		if (props.channels <= 0) throw new Error("invalid channel count");
		if ("samples" in props.size && props.size.samples <= 0) throw new Error("invalid frame samples");
		if ("duration" in props.size && props.size.duration <= 0) throw new Error("invalid frame duration");

		this.#sampleRate = props.sampleRate;
		this.#channels = props.channels;
		this.#size = props.size;
	}

	/** Append captured samples and return every complete codec frame they form. */
	push(input: AudioFrame): AudioFrame[] {
		if (input.channels.length !== this.#channels) throw new Error("wrong number of channels");

		const samples = input.channels[0]?.length ?? 0;
		for (const channel of input.channels) {
			if (channel.length !== samples) throw new Error("mismatching number of samples");
		}
		if (samples === 0) return [];

		// Output timestamps are counted forward from #origin, so a gap in the input would otherwise
		// be swallowed: the stream would slide earlier by the length of the gap and stay there.
		// Looping a file whose audio is shorter than its video hits this on every wrap, and the
		// error accumulates, so audio drifts further from video with each pass.
		if (this.#discontinuous(input.timestamp)) this.#reset(input.timestamp);

		this.#origin ??= input.timestamp;
		this.#inputSamples += samples;

		const output: AudioFrame[] = [];
		let offset = 0;
		while (offset < samples) {
			if (this.#buffer.length === 0) this.#buffer = this.#createBuffer();

			const remaining = this.#buffer[0].length - this.#written;
			const copied = Math.min(remaining, samples - offset);
			for (let channel = 0; channel < this.#channels; channel++) {
				this.#buffer[channel].set(input.channels[channel].subarray(offset, offset + copied), this.#written);
			}

			offset += copied;
			this.#written += copied;
			if (this.#written !== this.#buffer[0].length) continue;

			output.push({
				timestamp: this.#timestamp(),
				channels: this.#buffer,
			});
			this.#sampleIndex += this.#buffer[0].length;
			this.#frameIndex++;
			this.#buffer = [];
			this.#written = 0;
		}

		return output;
	}

	// Whether this chunk starts somewhere other than where the previous one left off.
	#discontinuous(timestamp: Time.Micro): boolean {
		if (this.#origin === undefined) return false;

		const elapsed = Time.Micro.fromSecond((this.#inputSamples / this.#sampleRate) as Time.Second);
		return Math.abs(timestamp - this.#origin - elapsed) > DISCONTINUITY;
	}

	// Start a new segment at `timestamp`, dropping the partial frame the previous one left behind.
	// Those samples sit on the old timeline, so padding them out with new ones would encode a splice
	// that never happened. It costs less than one frame of audio at the seam.
	#reset(timestamp: Time.Micro): void {
		this.#origin = timestamp;
		this.#frameIndex = 0;
		this.#sampleIndex = 0;
		this.#inputSamples = 0;
		this.#buffer = [];
		this.#written = 0;
	}

	#createBuffer(): Float32Array[] {
		const samples = this.#frameSamples();
		return Array.from({ length: this.#channels }, () => new Float32Array(samples));
	}

	#frameSamples(): number {
		if ("samples" in this.#size) return this.#size.samples;

		// Round cumulative boundaries instead of each frame independently. This alternates neighboring
		// integer sizes when a duration is a fractional number of source samples without accumulating drift.
		const end = Math.round(((this.#frameIndex + 1) * this.#size.duration * this.#sampleRate) / 1_000_000);
		return end - this.#sampleIndex;
	}

	#timestamp(): Time.Micro {
		if (this.#origin === undefined) throw new Error("missing timestamp origin");

		const offset =
			"duration" in this.#size
				? this.#frameIndex * this.#size.duration
				: Time.Micro.fromSecond((this.#sampleIndex / this.#sampleRate) as Time.Second);
		return (this.#origin + offset) as Time.Micro;
	}
}
