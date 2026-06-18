import { Time } from "@moq/net";

// Fallback ring capacity (ms) when buffered but no cap was supplied. The ceiling is always finite,
// so this is just defensive; the worklet drops the oldest samples once the ring fills.
const DEFAULT_MAX_BUFFER = 1_500 as Time.Milli;

export class AudioRingBuffer {
	#buffer: Float32Array[];
	#writeIndex = 0;
	#readIndex = 0;

	readonly rate: number;
	readonly channels: number;
	#stalled = true;

	// Buffered mode: anchor to the first sample, play through without skipping ahead.
	readonly #buffered: boolean;
	// Un-stall threshold in samples (how much to buffer before playback starts).
	#latencySamples: number;
	// Whether the read/write indices have been anchored to the first inserted sample.
	#anchored = false;

	constructor(props: {
		rate: number;
		channels: number;
		latency: Time.Milli;
		buffered?: boolean;
		maxBuffer?: Time.Milli;
	}) {
		if (props.channels <= 0) throw new Error("invalid channels");
		if (props.rate <= 0) throw new Error("invalid sample rate");
		if (props.latency <= 0) throw new Error("invalid latency");

		this.#latencySamples = Math.ceil(props.rate * Time.Second.fromMilli(props.latency));
		if (this.#latencySamples === 0) throw new Error("empty buffer");

		this.rate = props.rate;
		this.channels = props.channels;
		this.#buffered = props.buffered ?? false;

		// In buffered mode the capacity is the maxBuffer cap, not just the latency target.
		const maxBuffer = props.maxBuffer ?? DEFAULT_MAX_BUFFER;
		const capacity = this.#buffered
			? Math.ceil(props.rate * Time.Second.fromMilli(maxBuffer))
			: this.#latencySamples;

		this.#buffer = [];
		for (let i = 0; i < this.channels; i++) {
			this.#buffer[i] = new Float32Array(capacity);
		}
	}

	get stalled(): boolean {
		return this.#stalled;
	}

	get timestamp(): Time.Micro {
		return Time.Micro.fromSecond((this.#readIndex / this.rate) as Time.Second);
	}

	get length(): number {
		return this.#writeIndex - this.#readIndex;
	}

	get capacity(): number {
		return this.#buffer[0]?.length;
	}

	resize(latency: Time.Milli): void {
		// In buffered mode the capacity is fixed; latency only moves the un-stall threshold.
		if (this.#buffered) {
			this.#latencySamples = Math.ceil(this.rate * Time.Second.fromMilli(latency));
			return;
		}

		const newCapacity = Math.ceil(this.rate * Time.Second.fromMilli(latency));
		if (newCapacity === this.capacity) return;
		if (newCapacity === 0) throw new Error("empty buffer");

		const newBuffer: Float32Array[] = [];
		for (let i = 0; i < this.channels; i++) {
			newBuffer[i] = new Float32Array(newCapacity);
		}

		// Copy existing data, preserving the most recent samples
		const samplesToKeep = Math.min(this.length, newCapacity);
		if (samplesToKeep > 0) {
			// Copy the most recent samples (closest to writeIndex)
			const copyStart = this.#writeIndex - samplesToKeep;
			for (let channel = 0; channel < this.channels; channel++) {
				const src = this.#buffer[channel];
				const dst = newBuffer[channel];
				for (let i = 0; i < samplesToKeep; i++) {
					const srcPos = (copyStart + i) % src.length;
					const dstPos = (copyStart + i) % dst.length;
					dst[dstPos] = src[srcPos];
				}
			}
		}

		// Update state for the new buffer, only stall if empty.
		this.#buffer = newBuffer;
		this.#readIndex = this.#writeIndex - samplesToKeep;
		if (samplesToKeep === 0) this.#stalled = true;
	}

	write(timestamp: Time.Micro, data: Float32Array[]): void {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		let start = Math.round(Time.Second.fromMicro(timestamp) * this.rate);
		let samples = data[0].length;

		// Buffered mode: anchor both indices to the first sample so we play from its
		// timestamp instead of gap-filling silence from index 0 to a large timestamp.
		if (this.#buffered && !this.#anchored) {
			this.#readIndex = start;
			this.#writeIndex = start;
			this.#anchored = true;
		}

		// Ignore samples that are too old (before the read index)
		let offset = this.#readIndex - start;
		if (offset > samples) {
			// All samples are too old, ignore them
			return;
		} else if (offset > 0) {
			// Some samples are too old, skip them
			samples -= offset;
			start += offset;
		} else {
			offset = 0;
		}

		const end = start + samples;

		// Check if we need to discard old samples to prevent overflow
		const overflow = end - this.#readIndex - this.#buffer[0].length;
		if (overflow >= 0) {
			// Discard old samples and exit stalled mode
			this.#stalled = false;
			this.#readIndex += overflow;
		}

		// Fill gaps with zeros if there's a discontinuity
		if (start > this.#writeIndex) {
			const gapSize = Math.min(start - this.#writeIndex, this.#buffer[0].length);
			if (gapSize === 1) {
				console.warn("floating point inaccuracy detected");
			}

			for (let channel = 0; channel < this.channels; channel++) {
				const dst = this.#buffer[channel];
				for (let i = 0; i < gapSize; i++) {
					const writePos = (this.#writeIndex + i) % dst.length;
					dst[writePos] = 0;
				}
			}
		}

		// Write the actual samples
		for (let channel = 0; channel < this.channels; channel++) {
			let src = data[channel];
			src = src.subarray(src.length - samples);

			const dst = this.#buffer[channel];
			if (src.length !== samples) throw new Error("mismatching number of samples");

			for (let i = 0; i < samples; i++) {
				const writePos = (start + i) % dst.length;
				dst[writePos] = src[i];
			}
		}

		// Update write index, but only if we're moving forward
		if (end > this.#writeIndex) {
			this.#writeIndex = end;
		}

		// Start playback once we've buffered the latency target. In buffered mode the cap
		// is large, so we usually un-stall here rather than via the overflow path above.
		if (this.#buffered && this.length >= this.#latencySamples) {
			this.#stalled = false;
		}
	}

	// Flush all buffered samples and re-stall, ready to anchor the next utterance.
	reset(): void {
		this.#readIndex = 0;
		this.#writeIndex = 0;
		this.#stalled = true;
		this.#anchored = false;
	}

	read(output: Float32Array[]): number {
		if (output.length !== this.channels) throw new Error("wrong number of channels");
		if (this.#stalled) return 0;

		const samples = Math.min(this.#writeIndex - this.#readIndex, output[0].length);
		if (samples === 0) return 0;

		for (let channel = 0; channel < this.channels; channel++) {
			const dst = output[channel];
			const src = this.#buffer[channel];

			if (dst.length !== output[0].length) throw new Error("mismatching number of samples");

			for (let i = 0; i < samples; i++) {
				const readPos = (this.#readIndex + i) % src.length;
				dst[i] = src[readPos];
			}
		}

		this.#readIndex += samples;
		return samples;
	}
}
