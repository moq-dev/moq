import { Time } from "@moq/net";

export class AudioRingBuffer {
	#buffer: Float32Array[];
	#writeIndex = 0;
	#readIndex = 0;

	readonly rate: number;
	readonly channels: number;
	#stalled = true;
	#underruns = 0;

	// Samples in the most recent write, i.e. one decoded chunk. The overflow band tolerates this
	// much above the target before dropping, so a ring sitting exactly on the target doesn't drop
	// audio the moment the next chunk lands. The most recent write rather than a running maximum:
	// one oversized decode would otherwise widen the band for the life of the ring.
	#chunk = 0;

	// Buffered mode: play through everything buffered without skipping ahead.
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
	}) {
		if (props.channels <= 0) throw new Error("invalid channels");
		if (props.rate <= 0) throw new Error("invalid sample rate");
		if (props.latency <= 0) throw new Error("invalid latency");

		this.#latencySamples = Math.ceil(props.rate * Time.Second.fromMilli(props.latency));
		if (this.#latencySamples === 0) throw new Error("empty buffer");

		this.rate = props.rate;
		this.channels = props.channels;
		this.#buffered = props.buffered ?? false;

		// The ring holds the latency floor as PCM, with headroom above it. Sizing capacity to the
		// floor exactly makes the ring physically incapable of holding the slack the overflow band
		// wants, so every chunk that arrives while the ring is on target drops the oldest samples.
		// In buffered mode the headroom is also what keeps the backpressure-paced decode loop (on
		// the main thread) from overflow-dropping; the rest of the lookahead stays encoded upstream.
		const capacity = this.#capacityFor(this.#latencySamples);

		this.#buffer = [];
		for (let i = 0; i < this.channels; i++) {
			this.#buffer[i] = new Float32Array(capacity);
		}
	}

	#capacityFor(latencySamples: number): number {
		return latencySamples * 2;
	}

	get stalled(): boolean {
		return this.#stalled;
	}

	/** How many times the reader has run dry mid-playback. */
	get underruns(): number {
		return this.#underruns;
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
		this.#latencySamples = Math.ceil(this.rate * Time.Second.fromMilli(latency));

		const newCapacity = this.#capacityFor(this.#latencySamples);
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

		// Anchor both indices to the first sample so we play from its timestamp instead of
		// gap-filling silence from index 0 to a large timestamp, which would both waste the
		// ring on zeros and report a playhead a floor behind the first real sample.
		if (!this.#anchored) {
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
		this.#chunk = data[0].length;

		// Bound the ring. While playing, drop the oldest once it holds a whole chunk more than the
		// target and land back on the target: frames arrive one chunk at a time, so a ring sitting
		// exactly on the target is a chunk above it the moment the next one lands, and dropping on
		// that overshoot discards audio on every single write. While stalled the reader is not
		// consuming, so only the hard capacity applies; the band would throw away the very audio the
		// refill is accumulating. Buffered mode plays through everything, so it is capacity-bound too.
		const playing = !this.#stalled && !this.#buffered;
		const limit = playing ? Math.min(this.#latencySamples + this.#chunk, this.capacity) : this.capacity;
		if (end - this.#readIndex > limit) {
			this.#readIndex = end - (playing ? Math.min(this.#latencySamples, this.capacity) : this.capacity);
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

		// Start playback once we've buffered the latency target. This is the only way out of a
		// stall, so an underrun mid-playback refills to the target before resuming rather than
		// playing the next chunk on an empty cushion.
		if (this.length >= this.#latencySamples) {
			this.#stalled = false;
		}
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping whatever is already due.
	 *
	 * A successor track overwrites the slots its own samples land on, but anything the previous
	 * track wrote beyond them would otherwise still play once the successor runs out.
	 */
	truncate(timestamp: Time.Micro): void {
		const target = Math.round(Time.Second.fromMicro(timestamp) * this.rate);
		if (target >= this.#writeIndex) return;
		// Never retreat past the playhead: those samples are already due.
		this.#writeIndex = Math.max(target, this.#readIndex);
	}

	/**
	 * Hold playback until the ring holds the target again, keeping everything buffered.
	 *
	 * Used when the target deepens: `resize` alone only raises the bar a future refill has to clear,
	 * so a ring already playing keeps draining at its old depth and audio runs that much ahead of
	 * video. Parking the playhead spends exactly the deficit as silence and resumes on the same
	 * timeline, where `reset` would throw the buffer away and re-anchor.
	 */
	stall(): void {
		this.#stalled = true;
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
		if (samples <= 0) {
			// Ran dry mid-playback: re-stall so write() refills to the target before resuming.
			this.#stalled = true;
			this.#underruns++;
			return 0;
		}

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
