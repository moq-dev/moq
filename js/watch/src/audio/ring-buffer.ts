import { ringCapacity } from "./latency";
import type { Terms } from "./render";
import { type Decision, decide, HOLD, Stretch } from "./stretch";

// An AudioWorklet render quantum, the read size until the first read says otherwise.
const QUANTUM = 128;

/**
 * The worklet's PCM ring. Latencies are in milliseconds and timestamps in microseconds, as the port
 * carries them.
 *
 * Playback follows the rule in doc/concept/audio-jitter.md: received PCM plays faster or slower onto
 * the shared clock at a rate proportional to the playhead's lag behind it, and slower while the audio
 * received ahead of the playhead is about to run out. Audio trailing the clock past the bound skips
 * onto it, and a playhead ahead of it past the bound pads silence until the clock reaches it. A zero
 * bound turns the stretch off: received audio plays unaltered, and a playhead more than a window off
 * the clock skips ahead onto it or waits for it in silence.
 */
export class AudioRingBuffer {
	#buffer: Float32Array[];
	#writeIndex = 0;
	#readIndex = 0;

	readonly rate: number;
	readonly channels: number;
	#stalled = true;
	// Whether playback started since creation or the last reset, so running dry refills one stretch
	// window and a quantum rather than the floor.
	#started = false;
	#underruns = 0;
	#skips = 0;
	// Whether the last read skipped, so a run of reads skipping late audio counts once.
	#skipping = false;
	// Whether the last read padded silence toward the clock, which continues until the clock reaches the
	// playhead.
	#padding = false;
	// Whether the ring has read on the clock since the last reset. Until it has, nothing was heard off
	// the clock, so a join snaps onto it either way.
	#joined = false;
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
	// Declared silences still ahead of the playhead, oldest first, as [start, end) sample indices. The
	// ring holds them as zeros; the reader fills them with comfort noise. Each one follows a distinct
	// write, and every move of the playhead drops those behind it, so the ring's capacity bounds them.
	#pauses: Array<{ start: number; end: number }> = [];
	// The part of the last read inside a declared silence, as output indices.
	#quiet?: [number, number];
	// Whether the playhead ran out at a declared end and nothing has played since: intentional
	// silence, which is no underrun and survives the reset that re-anchors the next utterance.
	#silent = false;

	// Received PCM passes through the stretch on its way out. The read index is past what the stretch
	// holds ahead of the playhead.
	readonly #stretch: Stretch;
	// Whether the stretch holds a declared silence, which plays unstretched as comfort noise.
	#held = false;

	// Where the newest audio received ends, decoded or not, unset until some arrives on this timeline.
	#received?: number;
	// Where the shared clock stands at the first frame of the current read, unset without a clock.
	#clock?: number;
	// The size of the latest read, which a refill leaves room for.
	#period = QUANTUM;

	// Un-stall threshold in samples (how much to buffer before playback starts).
	#latencySamples: number;
	// How far audio may trail the clock, in samples. A zero bound turns the stretch off.
	#bound: number;
	// Whether the read/write indices have been anchored to the first inserted sample.
	#anchored = false;

	constructor(props: { rate: number; channels: number } & Pick<Terms, "latency"> & Partial<Terms>) {
		if (props.channels <= 0) throw new Error("invalid channels");
		if (props.rate <= 0) throw new Error("invalid sample rate");
		if (props.latency <= 0) throw new Error("invalid latency");

		this.#latencySamples = Math.ceil(props.rate * (props.latency / 1000));
		if (this.#latencySamples === 0) throw new Error("empty buffer");

		this.rate = props.rate;
		this.channels = props.channels;
		this.#bound = this.#samples(props.bound ?? 0);

		// The ring holds the latency floor as PCM, with headroom above it, at a capacity the receiver
		// fixes rather than the target: whatever of the target is above the floor stays encoded
		// upstream (the main thread holds it back), and the headroom keeps a burst from overflowing.
		const capacity = ringCapacity(this.rate);
		if (this.#latencySamples > capacity) throw new Error("latency exceeds capacity");

		this.#buffer = [];
		for (let i = 0; i < this.channels; i++) {
			this.#buffer[i] = new Float32Array(capacity);
		}
		this.#stretch = new Stretch(this.rate, this.channels);
	}

	#samples(ms: number): number {
		return Math.round((ms / 1000) * this.rate);
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
	get trimmed(): number {
		return (this.#trimmed / this.rate) * 1000;
	}

	/** Media the stretch removed by playing faster, cumulative. */
	get accelerated(): number {
		return (this.#accelerated / this.rate) * 1000;
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

	/** The playhead, with a crossfade's played share mapped onto the samples it shifts to. */
	get timestamp(): number {
		return (this.#playhead() / this.rate) * 1_000_000;
	}

	// The playhead in samples: the read index less what the stretch holds ahead of it.
	#playhead(): number {
		return this.#readIndex - this.#stretch.ahead;
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

	/**
	 * Move the floor, which never exceeds the fixed capacity, and the bound when given. A moved target
	 * moves the clock rather than the ring, which plays onto it like any other lag.
	 */
	setLatency(latency: number, terms?: Omit<Terms, "latency">): void {
		const latencySamples = Math.ceil(this.rate * (latency / 1000));
		if (latencySamples <= 0) throw new Error("empty buffer");
		if (latencySamples > this.capacity) throw new Error("latency exceeds capacity");
		this.#latencySamples = latencySamples;
		this.#bound = this.#samples(terms?.bound ?? 0);

		// Start a ring that the buffer already covers, which `write` would only notice on the next
		// frame, never for a source that has stopped.
		this.#resume();
	}

	/**
	 * Note that media ending at `timestamp` arrived, decoded or not. What lies past the playhead is
	 * the depth the stretch slows down to keep, and speeds up only as far as it leaves.
	 */
	received(timestamp: number): void {
		const end = Math.round((timestamp / 1_000_000) * this.rate);
		if (this.#received !== undefined && end <= this.#received) return;
		this.#received = end;
	}

	/** Where the shared clock stands at the first frame of the next read, in microseconds, if it has one. */
	clock(timestamp: number | undefined): void {
		this.#clock = timestamp === undefined ? undefined : Math.round((timestamp / 1_000_000) * this.rate);
	}

	// Un-stall once the buffer holds the floor to start on, or after running dry, one stretch window
	// and a read: the least that resumes without running dry again at once, and never more than the
	// floor. Slowing down then rebuilds the depth rather than a wait in silence.
	#resume(): void {
		if (!this.#stalled) return;
		const cushion = Math.min(this.#latencySamples, this.#stretch.window + this.#period);
		const need = this.#started ? cushion : this.#latencySamples;
		if (this.length < need) return;
		this.#stalled = false;
		this.#started = true;
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
		} else if (this.#contiguous(start)) {
			// A frame starting within a millisecond and two samples of where the last one ended
			// continues it: timestamps stamped in milliseconds, or rounded from another clock, land on
			// either side of the exact boundary, and a hole there would play a few samples of silence.
			start = this.#writeIndex;
		}

		// Ignore samples behind the read index, which runs ahead of the playhead by what the stretch
		// holds: that part is already on its way out, so a late write there is dropped too.
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

		// The next utterance's first audio: whatever lies between it and the previous utterance's audio
		// is declared silence.
		if (this.#next) {
			this.#next = false;
			this.#previous = undefined;
			if (start > this.#writeIndex) this.#pauses.push({ start: this.#writeIndex, end: start });
		}

		// Bound the ring at its capacity, dropping the oldest audio. The stretch keeps a ring near its
		// floor, so only a burst the decode gate let through overflows it.
		this.#prune();
		if (end - this.#readIndex > this.capacity) {
			// An empty ring only re-anchors over a gap it never held, which drops nothing.
			if (this.length > 0) this.#skips++;
			this.#skip(end - this.capacity, start);
		}

		// Fill gaps with zeros if there's a discontinuity
		if (start > this.#writeIndex) {
			const gapSize = Math.min(start - this.#writeIndex, this.#buffer[0].length);
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
		this.#snapEnd();
		// Decoded audio arrived too, if no message said so first.
		this.received((end / this.rate) * 1_000_000);
		this.#resume();
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping whatever is already due.
	 *
	 * A successor track overwrites the slots its own samples land on, but anything the previous
	 * track wrote beyond them would otherwise still play once the successor runs out.
	 */
	truncate(timestamp: number): void {
		const target = Math.round((timestamp / 1_000_000) * this.rate);
		if (target < this.#writeIndex) {
			// What the stretch holds runs the read index ahead of the playhead. Return it so the successor
			// replaces whatever of it is still unplayed, resuming the ring at the sample now playing.
			const stretch = this.#stretch;
			if (target < this.#readIndex && stretch.available > 0) {
				const capacity = this.#buffer[0].length;
				this.#readIndex = Math.max(Math.round(this.#playhead()), this.#writeIndex - capacity);
				stretch.clear();
			}
			// Never retreat past the playhead: those samples are already due.
			this.#writeIndex = Math.max(target, this.#readIndex);
			this.#pauses = this.#pauses.filter((pause) => pause.start < this.#writeIndex);
			const last = this.#pauses.at(-1);
			if (last) last.end = Math.min(last.end, this.#writeIndex);
		}
		// What arrived past the cut, decoded or not, was the predecessor's, so the successor's arrivals
		// count from there.
		const received = Math.max(target, this.#writeIndex);
		if (this.#received !== undefined && this.#received > received) this.#received = received;
	}

	/**
	 * Note that the publisher declared its audio ends at `timestamp`, so running dry there is silence
	 * rather than an underrun. Pass the end less any codec delay that leaves the decoded audio short
	 * of it.
	 */
	end(timestamp: number): void {
		this.#end = Math.round((timestamp / 1_000_000) * this.rate);
		this.#snapEnd();
		// The end can arrive after the playhead already ran out at it, or before any audio, as for a
		// listener that joins during a mute.
		if (!this.#anchored || (this.#stalled && this.length <= 0 && this.#ended())) this.#silent = true;
	}

	// Whether `index` is within a millisecond and two samples of the write index: the jitter of
	// timestamps stamped in milliseconds or rounded from another clock.
	#contiguous(index: number): boolean {
		return Math.abs(index - this.#writeIndex) <= this.rate / 1000 + 2;
	}

	// Move a declared end within that jitter of the written audio onto its end, since frames are placed
	// contiguously rather than at their stamps.
	#snapEnd(): void {
		if (this.#anchored && this.#end !== undefined && this.#contiguous(this.#end)) this.#end = this.#writeIndex;
	}

	// Move the read index forward to `index`, dropping any queued audio it passes, what the stretch
	// holds included, and the front of the incoming chunk at `start` when `index` lies past it. The
	// caller counts the skip; an empty ring only re-anchors over a gap it never held, so the trim does
	// not move, as in moq-audio.
	#skip(index: number, start: number): void {
		if (index > this.#readIndex && this.length > 0) this.#trimmed += this.#discarded(index, start);
		// What the stretch holds lies behind the new index, so it is dropped with the rest, as
		// moq-audio's skip discards it first.
		if (index > this.#readIndex) this.#stretch.clear();
		this.#readIndex = index;
		this.#prune();
	}

	// Media a skip to `index` drops: what the stretch holds past the playhead, what the ring holds up
	// to `index`, and the incoming chunk's front below it, less declared silence. The gap between the
	// write index and the chunk at `start` was never held, so it is no trim.
	#discarded(index: number, start: number): number {
		const stretch = this.#stretch;
		const held = Math.min(index, this.#writeIndex);
		const front = Math.max(start, this.#writeIndex);
		let dropped = held - this.#readIndex + Math.max(0, index - front);
		if (stretch.available > 0 && !this.#held) dropped += stretch.ahead;
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
		this.#started = false;
		this.#anchored = false;
		this.#end = undefined;
		this.#previous = undefined;
		this.#next = false;
		this.#pauses = [];
		this.#stretch.clear();
		this.#held = false;
		this.#received = undefined;
		this.#clock = undefined;
		this.#joined = false;
		this.#skipping = false;
		this.#padding = false;
	}

	/**
	 * Fill `output` from the playhead, returning how many frames it supplied. Pass where the shared
	 * clock stands at its first frame through {@link clock} beforehand.
	 */
	read(output: Float32Array[]): number {
		if (output.length !== this.channels) throw new Error("wrong number of channels");
		for (const dst of output) if (dst.length !== output[0].length) throw new Error("mismatching number of samples");
		this.#quiet = undefined;
		this.#period = output[0].length;
		if (this.#stalled) return 0;

		if (this.length <= 0) {
			// Ran dry mid-playback: re-stall so write() refills a cushion before resuming.
			this.#stalled = true;
			if (this.#ended()) this.#silent = true;
			else this.#underruns++;
			return 0;
		}

		// Snap onto the clock past the bound, or one window when that is larger: skip what trails it, and
		// pad silence ahead of a playhead that leads it until the clock arrives. Less than that is the
		// stretch's to play onto. Nothing was heard off the clock before a join, so a join snaps onto it
		// past the hop the stretch leaves alone, whichever way it is off. A finite tail plays as received.
		let skip = 0;
		let pad = 0;
		if (this.#clock !== undefined) {
			const lag = this.#clock - this.#playhead();
			const threshold = this.#joined ? Math.max(this.#bound, this.#stretch.window) : this.#stretch.hop;
			this.#joined = true;
			if (!this.#ending()) {
				if (lag > threshold) skip = lag;
				const lead = Math.round(-lag);
				this.#padding = lead > (this.#padding ? 0 : threshold);
				if (this.#padding) pad = Math.min(lead, output[0].length);
			}
		}
		if (skip > 0) {
			const index = Math.min(this.#writeIndex, Math.ceil(this.#playhead() + skip));
			if (!this.#skipping) this.#skips++;
			this.#skipping = true;
			this.#skip(index, this.#writeIndex);
			if (this.length <= 0) {
				this.#stalled = true;
				return 0;
			}
		} else {
			this.#skipping = false;
		}

		for (const dst of output) dst.fill(0, 0, pad);
		const stretch = this.#stretch;
		let samples = pad;
		while (samples < output[0].length) {
			this.#feed(output[0].length - samples, samples);
			if (stretch.due) {
				const decision = this.#decision(samples);
				const shift = stretch.next(decision);
				this.#accelerated += Math.max(0, shift);
			}
			const from = samples;
			samples += stretch.read(output, samples, output[0].length - samples);
			if (samples === from) break;
			// A quantum overlaps at most one declared silence unless an utterance is shorter than it,
			// which no codec frame is; the span then covers both silences and the sliver between them.
			if (this.#held) this.#quiet = [this.#quiet?.[0] ?? from, samples];
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

	// Whether the playhead is in audio before a declared end: a finite tail, which drains as received,
	// since the next utterance re-anchors the ring anyway and its depth says nothing about the clock.
	#ending(): boolean {
		const end = this.#previous ?? this.#end;
		return end !== undefined && this.#readIndex < end;
	}

	// Whether what the stretch plays may stretch: received audio, short of a declared end, on a clock,
	// with the stretch on.
	#stretching(): boolean {
		return !this.#held && !this.#ending() && this.#bound > 0 && this.#clock !== undefined;
	}

	// What the hop starting `elapsed` frames into this read decides.
	#decision(elapsed: number): Decision {
		if (!this.#stretching() || this.#clock === undefined) return HOLD;
		const playhead = this.#playhead();
		const depth = Math.max(this.#received ?? 0, this.#writeIndex) - playhead;
		const lag = this.#clock + elapsed - playhead;
		return decide(this.rate, { lag, depth, bound: this.#bound, read: this.#period });
	}

	// Top the stretch up off the ring: a window past the playhead while it may stretch, or else only
	// the `need` this read plays, so a write still lands anywhere past the playhead. What it holds is
	// either received audio or a declared silence, never both, so it never moves where one starts or
	// ends, and a splice never reaches across one. A playhead behind the clock skips a declared silence
	// by its lag instead, which drops no audio. `elapsed` frames of this read have played.
	#feed(need: number, elapsed: number): void {
		const stretch = this.#stretch;
		this.#prune();
		let pause = this.#pauses[0];
		const drained = stretch.available === 0;
		if (drained && pause !== undefined && pause.start <= this.#readIndex && this.#clock !== undefined) {
			const lag = this.#clock + elapsed - this.#readIndex;
			if (lag > 0) {
				this.#readIndex = Math.min(pause.end, this.#writeIndex, this.#readIndex + lag);
				this.#prune();
				pause = this.#pauses[0];
			}
		}

		const quiet = pause !== undefined && pause.start <= this.#readIndex;
		if (quiet !== this.#held) {
			if (!drained) return;
			stretch.forget();
			this.#held = quiet;
		}
		const want = this.#stretching() ? stretch.window : need;
		let frames = Math.min(this.#writeIndex - this.#readIndex, want - stretch.queued, stretch.room());
		if (pause) frames = Math.min(frames, (quiet ? pause.end : pause.start) - this.#readIndex);
		if (frames <= 0) return;
		stretch.push(this.#buffer, this.#readIndex, frames);
		this.#readIndex += frames;
	}
}
