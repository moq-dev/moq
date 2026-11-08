import { ringCapacity } from "./latency";

/** The worklet's PCM ring. Latencies are in milliseconds and timestamps in microseconds, as the port carries them. */
export class AudioRingBuffer {
	#buffer: Float32Array[];
	#writeIndex = 0;
	#readIndex = 0;

	readonly rate: number;
	readonly channels: number;
	#stalled = true;
	// Held stalled for the part of the target above the floor, until the next floor update.
	#parked = false;
	#underruns = 0;

	// Where the publisher declared its audio ends, as a sample index, until the next reset.
	#end?: number;
	// That end once the next utterance follows it while its audio still plays, until the next
	// utterance's first write lands: running dry there before it does is still declared silence.
	#previous?: number;
	// Set by `next` until that first write, which marks where the previous utterance's audio stops.
	#next = false;
	// Where the audio before the latest declared silence stops. The silence is declared, so it must not
	// count toward the live band and push the unplayed tail out.
	#tail?: number;
	// Declared silences still ahead of the playhead, oldest first, as [start, end) sample indices. The
	// ring holds them as zeros; the reader fills them with comfort noise. Each one follows a distinct
	// write, and every move of the playhead drops those behind it, so the ring's capacity bounds them.
	#pauses: Array<{ start: number; end: number }> = [];
	// The part of the last read inside a declared silence, as output indices.
	#quiet?: [number, number];
	// Whether the playhead ran out at a declared end and nothing has played since: intentional
	// silence, which is no underrun and survives the reset that re-anchors the next utterance.
	#silent = false;

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
		latency: number;
		buffered?: boolean;
	}) {
		if (props.channels <= 0) throw new Error("invalid channels");
		if (props.rate <= 0) throw new Error("invalid sample rate");
		if (props.latency <= 0) throw new Error("invalid latency");

		this.#latencySamples = Math.ceil(props.rate * (props.latency / 1000));
		if (this.#latencySamples === 0) throw new Error("empty buffer");

		this.rate = props.rate;
		this.channels = props.channels;
		this.#buffered = props.buffered ?? false;

		// The ring holds the latency floor as PCM, with headroom above it, at a capacity the receiver
		// fixes rather than the target: whatever of the target is above the floor stays encoded
		// upstream (the main thread holds it back). The headroom tolerates a chunk above the floor in
		// live mode, and in buffered mode keeps the paced decode loop from overflow-dropping.
		const capacity = ringCapacity(this.rate);
		if (this.#latencySamples > capacity) throw new Error("latency exceeds capacity");

		this.#buffer = [];
		for (let i = 0; i < this.channels; i++) {
			this.#buffer[i] = new Float32Array(capacity);
		}
	}

	get stalled(): boolean {
		return this.#stalled;
	}

	/** How many times the reader has run dry mid-playback. */
	get underruns(): number {
		return this.#underruns;
	}

	/** Whether playback is at a declared end, reached or announced before any audio, and nothing has played since. */
	get silent(): boolean {
		return this.#silent;
	}

	/**
	 * The part of the last read that lies in a declared silence between two utterances, as
	 * `[from, to)` output indices. Those samples are zeros for the reader to fill with comfort noise.
	 */
	get quiet(): readonly [number, number] | undefined {
		return this.#quiet;
	}

	get timestamp(): number {
		return (this.#readIndex / this.rate) * 1_000_000;
	}

	/** Whether an insert has anchored the indices since creation or the last reset. */
	get anchored(): boolean {
		return this.#anchored;
	}

	get length(): number {
		return this.#writeIndex - this.#readIndex;
	}

	get capacity(): number {
		return this.#buffer[0]?.length;
	}

	/** Move the floor, which never exceeds the fixed capacity, and end a park. */
	setLatency(latency: number): void {
		const latencySamples = Math.ceil(this.rate * (latency / 1000));
		if (latencySamples <= 0) throw new Error("empty buffer");
		if (latencySamples > this.capacity) throw new Error("latency exceeds capacity");

		// A deeper floor parks playback until it refills. Video holds the extra delay on its own, so
		// audio that kept draining at the old depth would run ahead by the difference. Parking keeps
		// what is buffered, so a rise costs only its own size in silence, and none if the buffer
		// already covers the new floor.
		if (latencySamples > this.#latencySamples) this.#stalled = true;
		this.#latencySamples = latencySamples;
		this.#parked = false;

		// Resume a refill that the buffer already covers, which `write` would only notice on the
		// next frame, never for a source that has stopped.
		if (this.length >= latencySamples) this.#stalled = false;
	}

	/**
	 * Hold playback until the next floor update, keeping what is buffered.
	 *
	 * For a rise in the part of the target held encoded upstream: the ring already covers its floor,
	 * so only the main thread knows when the received media covers the rest. A write does not resume
	 * it, since output decoded before the park may still land.
	 */
	park(): void {
		this.#stalled = true;
		this.#parked = true;
	}

	write(timestamp: number, data: Float32Array[]): void {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		let start = Math.round((timestamp / 1_000_000) * this.rate);
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

		// The next utterance's first audio: whatever lies between it and the previous utterance's audio
		// is declared silence.
		if (this.#next) {
			this.#next = false;
			this.#previous = undefined;
			this.#tail = this.#writeIndex;
			if (start > this.#writeIndex) this.#pauses.push({ start: this.#writeIndex, end: start });
		}

		// Bound the ring. While playing, drop the oldest once it holds a whole chunk more than the
		// target and land back on the target: frames arrive one chunk at a time, so a ring sitting
		// exactly on the target is a chunk above it the moment the next one lands, and dropping on
		// that overshoot discards audio on every single write. While stalled the reader is not
		// consuming, so only the hard capacity applies; the band would throw away the very audio the
		// refill is accumulating. Buffered mode plays through everything, so it is capacity-bound too.
		const playing = !this.#stalled && !this.#buffered;
		// Until the tail before a declared silence has played, only the capacity bounds the ring; the
		// band then catches up by skipping the silence.
		const band = playing && (this.#tail === undefined || this.#readIndex >= this.#tail);
		const limit = band ? Math.min(this.#latencySamples + this.#chunk, this.capacity) : this.capacity;
		if (end - this.#readIndex > limit) {
			this.#readIndex = end - (band ? Math.min(this.#latencySamples, this.capacity) : this.capacity);
			this.#prune();
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
		// stall but a park, so an underrun mid-playback refills to the target before resuming rather
		// than playing the next chunk on an empty cushion.
		if (!this.#parked && this.length >= this.#latencySamples) {
			this.#stalled = false;
		}
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping whatever is already due.
	 *
	 * A successor track overwrites the slots its own samples land on, but anything the previous
	 * track wrote beyond them would otherwise still play once the successor runs out.
	 */
	truncate(timestamp: number): void {
		const target = Math.round((timestamp / 1_000_000) * this.rate);
		if (target >= this.#writeIndex) return;
		// Never retreat past the playhead: those samples are already due.
		this.#writeIndex = Math.max(target, this.#readIndex);
		this.#pauses = this.#pauses.filter((pause) => pause.start < this.#writeIndex);
		const last = this.#pauses.at(-1);
		if (last) last.end = Math.min(last.end, this.#writeIndex);
	}

	/**
	 * Note that the publisher declared its audio ends at `timestamp`, so running dry there is silence
	 * rather than an underrun. Pass the end less any codec delay that leaves the decoded audio short
	 * of it.
	 */
	end(timestamp: number): void {
		this.#end = Math.round((timestamp / 1_000_000) * this.rate);
		// The end can arrive after the playhead already ran out at it, or before any audio, as for a
		// listener that joins during a mute.
		if (!this.#anchored || (this.#stalled && this.length <= 0 && this.#ended())) this.#silent = true;
	}

	// Forget the declared silences the playhead has passed.
	#prune(): void {
		const pauses = this.#pauses;
		let passed = 0;
		while (passed < pauses.length && pauses[passed].end <= this.#readIndex) passed++;
		if (passed > 0) pauses.splice(0, passed);
	}

	// Whether the playhead has reached the declared end.
	#ended(): boolean {
		const end = this.#previous ?? this.#end;
		return end !== undefined && this.#readIndex >= end;
	}

	/**
	 * The publisher's next utterance follows its declared end. Once nothing is left to play, flush
	 * and re-anchor on it like {@link reset}; while the end's audio is still buffered, play straight
	 * into the next utterance on the same timeline, dropping nothing, with the silence between them
	 * reported through {@link quiet}.
	 */
	next(): void {
		if (!this.#anchored || this.length <= 0) {
			const silent = this.#silent;
			this.reset();
			this.#silent = silent;
			return;
		}
		this.#previous = this.#end ?? this.#previous;
		this.#end = undefined;
		this.#next = true;
	}

	// Flush all buffered samples and re-stall, ready to anchor the next utterance.
	reset(): void {
		this.#silent = false;
		this.#readIndex = 0;
		this.#writeIndex = 0;
		this.#stalled = true;
		this.#parked = false;
		this.#anchored = false;
		this.#end = undefined;
		this.#previous = undefined;
		this.#next = false;
		this.#tail = undefined;
		this.#pauses = [];
	}

	read(output: Float32Array[]): number {
		if (output.length !== this.channels) throw new Error("wrong number of channels");
		this.#quiet = undefined;
		if (this.#stalled) return 0;

		const samples = Math.min(this.#writeIndex - this.#readIndex, output[0].length);
		if (samples <= 0) {
			// Ran dry mid-playback: re-stall so write() refills to the target before resuming.
			this.#stalled = true;
			if (this.#ended()) this.#silent = true;
			else this.#underruns++;
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

		// A quantum overlaps at most one declared silence unless an utterance is shorter than it, which
		// no codec frame is; the span then covers both silences and the sliver between them.
		this.#prune();
		const pauses = this.#pauses;
		const after = this.#readIndex + samples;
		const first = pauses[0];
		if (first && first.start < after) {
			let last = first;
			for (const pause of pauses) if (pause.start < after) last = pause;
			this.#quiet = [Math.max(0, first.start - this.#readIndex), Math.min(samples, last.end - this.#readIndex)];
		}

		this.#readIndex += samples;
		this.#silent = false;

		// A short quantum ends in silence, so it counts as an underrun even if a chunk lands before
		// the next read. It does not park playback: the shortfall is under one quantum, and a
		// refill would spend the whole target as silence to cover it. At a declared end it is the
		// silence the publisher announced, and the ring waits for the next utterance.
		if (samples < output[0].length) {
			if (this.#ended()) {
				this.#silent = true;
				this.#stalled = true;
			} else {
				this.#underruns++;
			}
		}

		return samples;
	}
}
