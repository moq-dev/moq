import { Micro, Milli, Second } from "@moq/clock";
import { ringCapacity } from "./latency";
import { Stretch } from "./stretch";

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
	#skips = 0;
	// Samples of media a skip dropped, and that the stretch removed by accelerating, gap zeros
	// included and declared silence excluded, as moq-audio counts them.
	#trimmed = 0;
	#accelerated = 0;

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

	// Samples in the most recent write, i.e. one decoded chunk. Catching up over a declared silence
	// tolerates this much above the target, so a ring sitting exactly on the target doesn't skip the
	// moment the next chunk lands. The most recent write rather than a running maximum: one oversized
	// decode would otherwise widen the band for the life of the ring.
	#chunk = 0;

	// Live playback converges on the floor by accelerating each window of received PCM on its way out.
	// The read index is past the staged window, which spans `#window` samples of the ring.
	readonly #stretch: Stretch;
	#window = 0;
	// The ring's length right after the latest write, less what the stretch has removed since. That
	// is where depth reads the clock: a frame has just arrived. Between arrivals the ring drains
	// toward the next one, which says nothing about how far behind the playhead is, so steering on
	// the current length would expand into every gap and settle behind the shared clock.
	#peak = 0;
	// Whether the staged window lies in a declared silence, which plays unstretched as comfort noise.
	#windowQuiet = false;

	// Buffered mode: play through everything buffered without skipping ahead.
	readonly #buffered: boolean;
	// Un-stall threshold in samples (how much to buffer before playback starts).
	#latencySamples: number;
	// Whether the read/write indices have been anchored to the first inserted sample.
	#anchored = false;

	constructor(props: {
		rate: number;
		channels: number;
		latency: Milli;
		buffered?: boolean;
	}) {
		if (props.channels <= 0) throw new Error("invalid channels");
		if (props.rate <= 0) throw new Error("invalid sample rate");
		if (props.latency <= 0) throw new Error("invalid latency");

		this.#latencySamples = Math.ceil(props.rate * Second.fromMilli(props.latency));
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
		this.#stretch = new Stretch(this.rate, this.channels);
	}

	get stalled(): boolean {
		return this.#stalled;
	}

	/** How many times the reader has run dry mid-playback. */
	get underruns(): number {
		return this.#underruns;
	}

	/** How many times the ring skipped ahead, dropping queued audio rather than playing it. */
	get skips(): number {
		return this.#skips;
	}

	/** Media the ring's skips dropped unplayed, less declared silence, cumulative. */
	get trimmed(): Milli {
		return Milli.fromSecond((this.#trimmed / this.rate) as Second);
	}

	/** Media the stretch removed by playing faster, cumulative. */
	get accelerated(): Milli {
		return Milli.fromSecond((this.#accelerated / this.rate) as Second);
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

	/** The playhead, with the staged window's unplayed output mapped back onto the samples it came from. */
	get timestamp(): Micro {
		const stretch = this.#stretch;
		const staged = stretch.length > 0 ? (stretch.available * this.#window) / stretch.length : 0;
		return Micro.fromSecond(((this.#readIndex - staged) / this.rate) as Second);
	}

	/** Whether an insert has anchored the indices since creation or the last reset. */
	get anchored(): boolean {
		return this.#anchored;
	}

	/** Samples left to play: the ring past the read index, plus the stretched output staged from it. */
	get length(): number {
		return this.#writeIndex - this.#readIndex + this.#stretch.available;
	}

	get capacity(): number {
		return this.#buffer[0]?.length;
	}

	/** Move the floor, which never exceeds the fixed capacity, and end a park. */
	setLatency(latency: Milli): void {
		const latencySamples = Math.ceil(this.rate * Second.fromMilli(latency));
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

	write(timestamp: Micro, data: Float32Array[]): void {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		let start = Math.round(Second.fromMicro(timestamp) * this.rate);
		let samples = data[0].length;

		// Anchor both indices to the first sample so we play from its timestamp instead of
		// gap-filling silence from index 0 to a large timestamp, which would both waste the
		// ring on zeros and report a playhead a floor behind the first real sample.
		if (!this.#anchored) {
			this.#readIndex = start;
			this.#writeIndex = start;
			this.#anchored = true;
		}

		// Ignore samples behind the read index, which runs ahead of the playhead by the staged window:
		// that part is already staged to play, so a late write there is dropped too.
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

		// Bound the ring. While playing live, the read path accelerates the ring back onto its floor, so
		// dropping the oldest audio and landing on the floor is an emergency re-anchor, as moq-audio
		// does: at NetEq's fast threshold, four times the floor plus 20 ms, or once the ring holds three
		// quarters of its headroom above the floor, whichever comes first. While stalled the
		// reader is not consuming, so only the hard capacity applies; dropping would throw away the very
		// audio the refill is accumulating. Buffered mode plays through everything, so it is
		// capacity-bound too.
		const playing = !this.#stalled && !this.#buffered;
		// Until the tail before a declared silence has played, only the capacity bounds the ring; then
		// a ring a chunk above its floor catches up by skipping the silence, which drops no audio.
		const band = playing && (this.#tail === undefined || this.#readIndex >= this.#tail);
		this.#prune();
		const pause = this.#pauses[0];
		const latency = this.#latencySamples;
		const queued = end - this.#readIndex + this.#stretch.available;
		if (end - this.#readIndex > this.capacity) {
			this.#skip(end - this.capacity, start);
		} else if (band && pause && pause.start <= this.#readIndex && queued > latency + this.#chunk) {
			// Skip only the silence past the queued floor, which the staged window counts toward, so the
			// read index only moves forward and never re-stages PCM the window already holds.
			this.#readIndex = Math.min(pause.end, this.#readIndex + queued - latency);
			this.#prune();
		} else if (
			band &&
			queued >=
				Math.min(
					4 * (latency + Math.floor((this.rate * 20) / 1000)),
					latency + Math.floor(((this.capacity - latency) * 3) / 4),
				)
		) {
			this.#skip(end - latency, start);
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
		this.#peak = this.length;

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
	truncate(timestamp: Micro): void {
		const target = Math.round(Second.fromMicro(timestamp) * this.rate);
		if (target >= this.#writeIndex) return;
		const length = this.length;
		// A staged window runs the read index ahead of the playhead. Unstage it so the successor
		// replaces whatever of it is still unplayed, resuming the ring at the sample now playing.
		const stretch = this.#stretch;
		if (target < this.#readIndex && stretch.available > 0) {
			const staged = (stretch.available * this.#window) / stretch.length;
			const capacity = this.#buffer[0].length;
			this.#readIndex = Math.max(Math.round(this.#readIndex - staged), this.#writeIndex - capacity);
			stretch.clear();
			this.#window = 0;
		}
		// Never retreat past the playhead: those samples are already due.
		this.#writeIndex = Math.max(target, this.#readIndex);
		this.#pauses = this.#pauses.filter((pause) => pause.start < this.#writeIndex);
		const last = this.#pauses.at(-1);
		if (last) last.end = Math.min(last.end, this.#writeIndex);
		this.#peak = Math.max(0, this.#peak - (length - this.length));
	}

	/**
	 * Note that the publisher declared its audio ends at `timestamp`, so running dry there is silence
	 * rather than an underrun. Pass the end less any codec delay that leaves the decoded audio short
	 * of it.
	 */
	end(timestamp: Micro): void {
		this.#end = Math.round(Second.fromMicro(timestamp) * this.rate);
		// The end can arrive after the playhead already ran out at it, or before any audio, as for a
		// listener that joins during a mute.
		if (!this.#anchored || (this.#stalled && this.length <= 0 && this.#ended())) this.#silent = true;
	}

	// Move the read index forward to `index`, dropping any queued audio it passes, staged included,
	// and the front of the incoming chunk at `start` when `index` lies past it. An empty ring only
	// re-anchors over a gap it never held: neither counter moves, as in moq-audio.
	#skip(index: number, start: number): void {
		if (index > this.#readIndex && this.length > 0) {
			this.#skips++;
			this.#trimmed += this.#discarded(index, start);
		}
		// The staged window lies behind the new index, so it is dropped with the rest, as moq-audio's
		// trim discards staged output first.
		const length = this.length;
		if (index > this.#readIndex) {
			this.#stretch.clear();
			this.#window = 0;
		}
		this.#readIndex = index;
		this.#peak = Math.max(0, this.#peak - (length - this.length));
		this.#prune();
	}

	// Media a skip to `index` drops: the unplayed share of the staged window, what the ring holds up
	// to `index`, and the incoming chunk's front below it, less declared silence. The gap between the
	// write index and the chunk at `start` was never held, so it is no trim.
	#discarded(index: number, start: number): number {
		const stretch = this.#stretch;
		const held = Math.min(index, this.#writeIndex);
		const front = Math.max(start, this.#writeIndex);
		let dropped = held - this.#readIndex + Math.max(0, index - front);
		if (stretch.available > 0 && !this.#windowQuiet) dropped += (stretch.available * this.#window) / stretch.length;
		for (const pause of this.#pauses) {
			dropped -= Math.max(0, Math.min(held, pause.end) - Math.max(this.#readIndex, pause.start));
		}
		return dropped;
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
		this.#stretch.clear();
		this.#window = 0;
		this.#peak = 0;
	}

	read(output: Float32Array[]): number {
		if (output.length !== this.channels) throw new Error("wrong number of channels");
		for (const dst of output) if (dst.length !== output[0].length) throw new Error("mismatching number of samples");
		this.#quiet = undefined;
		if (this.#stalled) return 0;

		if (this.length <= 0) {
			// Ran dry mid-playback: re-stall so write() refills to the target before resuming.
			this.#stalled = true;
			if (this.#ended()) this.#silent = true;
			else this.#underruns++;
			return 0;
		}

		const stretch = this.#stretch;
		let samples = 0;
		while (samples < output[0].length) {
			if (stretch.available === 0 && !this.#stage(output[0].length - samples)) break;
			const from = samples;
			samples += stretch.read(output, samples);
			// A quantum overlaps at most one declared silence unless an utterance is shorter than it,
			// which no codec frame is; the span then covers both silences and the sliver between them.
			if (this.#windowQuiet) this.#quiet = [this.#quiet?.[0] ?? from, samples];
		}
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

	// Move the next window off the ring into the stretch, which accelerates a live ring onto its floor.
	// A window stops at the edge of a declared silence, which plays unstretched, so the stretch never
	// moves where one starts or ends. Whatever cannot stretch stages only the `need` this quantum
	// reads, so a write still lands anywhere past the playhead. Returns false once the ring is empty.
	#stage(need: number): boolean {
		const buffered = this.#writeIndex - this.#readIndex;
		if (buffered <= 0) return false;

		this.#prune();
		const pause = this.#pauses[0];
		const quiet = pause !== undefined && pause.start <= this.#readIndex;
		// Audio before a declared end is a finite tail: it drains as received, since the next utterance
		// re-anchors the ring anyway.
		const end = this.#previous ?? this.#end;
		const ending = end !== undefined && this.#readIndex < end;
		const enabled = !quiet && !ending && !this.#buffered;
		let frames = Math.min(buffered, this.#stretch.window, enabled ? buffered : need);
		if (pause) frames = Math.min(frames, (quiet ? pause.end : pause.start) - this.#readIndex);

		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#buffer[channel];
			const dst = this.#stretch.input[channel];
			for (let i = 0; i < frames; i++) dst[i] = src[(this.#readIndex + i) % src.length];
		}
		const stretch = this.#stretch;
		stretch.process(frames, this.#peak - this.#latencySamples, enabled);
		this.#peak -= frames - stretch.length;
		this.#accelerated += Math.max(0, frames - stretch.length);
		this.#readIndex += frames;
		this.#window = frames;
		this.#windowQuiet = quiet;
		return true;
	}
}
