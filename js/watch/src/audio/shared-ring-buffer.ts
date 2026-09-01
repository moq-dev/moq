import { Time } from "@moq/net";

// Control array slot indices.
//
// Every slot has exactly one writer, which is what keeps the ring correct without locks. The
// playhead is the one value both sides need and neither can own, so it is not stored: it is the
// difference of two slots, `CONSUMED - ANCHOR`, each owned by one side.
const WRITE = 0; // main thread
const CONSUMED = 1; // worklet: total samples it has ever played, monotonic, never reset
const ANCHOR = 2; // main thread: the CONSUMED value that the current timeline calls position 0
const LATENCY = 3; // main thread
const STALLED = 4; // main thread
// main thread: bumped on every re-anchor. Only ever compared for equality by the reader, never
// used to guard a write, so it cannot repeat the ABA that came with guarding a shared cursor.
const EPOCH = 5; // main thread
const CONTROL_SLOTS = 6;

export interface SharedRingBufferInit {
	channels: number;
	capacity: number; // samples per channel, always a power of two
	rate: number;
	samples: SharedArrayBuffer; // channels * capacity * Float32Array.BYTES_PER_ELEMENT bytes
	control: SharedArrayBuffer; // CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT bytes
	// Buffered mode: never skip ahead on read, so we play through everything buffered.
	buffered: boolean;
}

/** Rounds up to a power of two, which is what makes `slot` wrap-invariant. */
function ceilPow2(n: number): number {
	return n <= 1 ? 1 : 1 << (32 - Math.clz32(n - 1));
}

/**
 * Allocate the shared memory for a ring holding at least `capacity` samples per channel.
 *
 * The capacity is rounded up to a power of two so `slot` can mask instead of taking a
 * remainder; read `init.capacity` back rather than assuming the requested value.
 */
export function allocSharedRingBuffer(
	channels: number,
	capacity: number,
	rate: number,
	buffered = false,
): SharedRingBufferInit {
	if (channels <= 0) throw new Error("invalid channels");
	if (capacity <= 0 || capacity > 2 ** 30) throw new Error("invalid capacity");
	if (rate <= 0) throw new Error("invalid sample rate");

	capacity = ceilPow2(capacity);

	const samples = new SharedArrayBuffer(channels * capacity * Float32Array.BYTES_PER_ELEMENT);
	const control = new SharedArrayBuffer(CONTROL_SLOTS * Int32Array.BYTES_PER_ELEMENT);

	// Initialize STALLED to 1
	const ctrl = new Int32Array(control);
	Atomics.store(ctrl, STALLED, 1);

	return { channels, capacity, rate, samples, control, buffered };
}

/** Modular i32 max: returns a if a is ahead of b, else b. */
function i32Max(a: number, b: number): number {
	return ((a - b) | 0) > 0 ? a : b;
}

/**
 * Maps a sample index to a [0, capacity) array slot.
 *
 * `capacity` is a power of two, so masking is exact for negative indexes and, unlike a
 * remainder, survives the i32 wrap: consecutive indexes stay consecutive slots across
 * 0x7fffffff -> 0x80000000, which is what keeps unread samples from aliasing there.
 */
function slot(idx: number, capacity: number): number {
	return idx & (capacity - 1);
}

export class SharedRingBuffer {
	readonly channels: number;
	readonly capacity: number;
	readonly rate: number;
	readonly buffered: boolean;
	readonly init: SharedRingBufferInit;

	#control: Int32Array;
	#samples: Float32Array[];

	// Whether READ/WRITE have been anchored to the first inserted sample.
	#anchored = false;

	// Absolute sample index of that first sample. READ/WRITE are stored relative to it, so
	// `timestamp` adds it back to recover media time. Main-thread only: the worklet reads by
	// difference and never needs an absolute position.
	#anchor = 0;

	// Unwrapped READ, in anchor-relative samples, plus the raw i32 it was last derived from.
	// READ is Int32 and wraps every ~13.5h at 44.1kHz. That is harmless for the modular
	// comparisons the ring runs on, but reading the negative half as a magnitude would report
	// the playhead jumping back 2^32 samples, so accumulate deltas here instead. Main-thread
	// only, and only accurate while `timestamp` is observed more often than READ can advance
	// 2^31 samples, which the 50ms poll in `buffer.ts` satisfies by six orders of magnitude.
	#position = 0;
	#lastRead = 0;

	// Total samples this reader has played. Worklet-only, and the only slot it writes, so a plain
	// store is enough: no other thread ever advances it.
	#consumed: number;

	/**
	 * Wrap the shared memory described by `init`.
	 *
	 * Pass `previous` in the worklet when this ring replaces one already being read. `resize`
	 * snapshots the playhead on the main thread and hands the replacement over by message, so the
	 * worklet keeps draining the old ring in the meantime. Carrying its running count across means
	 * those samples are already accounted for and never replay, with no shared cursor to reconcile.
	 */
	constructor(init: SharedRingBufferInit, previous?: SharedRingBuffer) {
		this.channels = init.channels;
		this.capacity = init.capacity;
		this.rate = init.rate;
		this.buffered = init.buffered;
		this.init = init;

		this.#control = new Int32Array(init.control);
		this.#samples = [];
		for (let i = 0; i < this.channels; i++) {
			this.#samples.push(
				new Float32Array(init.samples, i * this.capacity * Float32Array.BYTES_PER_ELEMENT, this.capacity),
			);
		}

		// The worklet's own tally, which outlives any single ring. `resize` seeds the replacement
		// with the count it snapshotted, but the reader may have played more since, so a handoff
		// keeps the reader's live value rather than the seed, and publishes it right away so the
		// writer's view of the playhead does not lag a quantum behind.
		//
		// Only within the same epoch, though: samples played from the old ring say nothing about a
		// timeline that re-anchored after the snapshot, and carrying them forward would start the
		// replacement past audio it has never played.
		this.#consumed = Atomics.load(this.#control, CONSUMED);
		if (previous !== undefined && Atomics.load(previous.#control, EPOCH) === Atomics.load(this.#control, EPOCH)) {
			this.#publish(previous.#consumed);
		}
	}

	/**
	 * Insert audio samples at the given timestamp.
	 * Main thread only. Handles out-of-order writes, gap filling, and overflow.
	 */
	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		if (data.length !== this.channels) throw new Error("wrong number of channels");

		let start = Math.round(Time.Second.fromMicro(timestamp) * this.rate);
		const originalLength = data[0].length;
		let offset = 0;

		// Anchor to the first sample so playback starts at its timestamp rather than gap-filling
		// from index 0.
		if (!this.#anchored) {
			// Rebase onto the new timeline by moving ANCHOR up to whatever the reader has played,
			// which puts the playhead back at zero without writing a slot the reader owns.
			this.#anchor = start;
			Atomics.add(this.#control, EPOCH, 1);
			Atomics.store(this.#control, ANCHOR, Atomics.load(this.#control, CONSUMED));
			Atomics.store(this.#control, WRITE, 0);
			this.#anchored = true;
			this.#position = 0;
			this.#lastRead = 0;
		}

		// Positions are relative to the anchor. READ/WRITE are Int32, so an absolute sample index
		// wraps once a stream has been broadcasting a while, and the wrapped value reads as far
		// ahead of READ, so every insert would be discarded as too old and the ring would go
		// silent for good. Relative positions start at 0 instead. They still wrap, after ~13.5h at
		// 44.1kHz, but nothing here reads them as magnitudes: the comparisons are modular, `slot`
		// masks a power-of-two capacity, and `timestamp` keeps its own unwrapped position.
		start = (start - this.#anchor) | 0;

		const end = (start + originalLength) | 0;

		// Trim old: discard samples before the read index
		const read = this.#read();
		const behind = (read - start) | 0;
		if (behind > 0) {
			if (behind >= originalLength) {
				// All samples are too old
				return;
			}
			offset = behind;
			start = (start + behind) | 0;
		}

		const samples = originalLength - offset;

		// Overflow: if the write would exceed capacity from the playhead, drop the oldest samples.
		if (((end - read) | 0) > this.capacity) {
			this.#seek((end - this.capacity) | 0);
		}

		// Gap fill: zero-fill from current WRITE to start if there's a discontinuity
		const write = Atomics.load(this.#control, WRITE);
		const gap = (start - write) | 0;
		if (gap > 0) {
			const gapSize = Math.min(gap, this.capacity);
			for (let channel = 0; channel < this.channels; channel++) {
				const dst = this.#samples[channel];
				for (let i = 0; i < gapSize; i++) {
					dst[slot((write + i) | 0, this.capacity)] = 0;
				}
			}
		}

		// Write sample data
		for (let channel = 0; channel < this.channels; channel++) {
			const src = data[channel];
			const dst = this.#samples[channel];
			for (let i = 0; i < samples; i++) {
				dst[slot((start + i) | 0, this.capacity)] = src[offset + i];
			}
		}

		// Advance WRITE (only forward)
		Atomics.store(this.#control, WRITE, i32Max(Atomics.load(this.#control, WRITE), end));

		// Un-stall: if buffered data >= LATENCY
		const currentRead = this.#read();
		const currentWrite = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);
		if (((currentWrite - currentRead) | 0) >= latency && latency > 0) {
			Atomics.store(this.#control, STALLED, 0);
		}
	}

	/**
	 * Read audio samples into the output buffers.
	 * AudioWorklet only. Returns the number of samples read.
	 */
	read(output: Float32Array[]): number {
		if (Atomics.load(this.#control, STALLED) === 1) return 0;

		// STALLED is only checked here, so a reset plus a re-anchoring insert can rebase the
		// timeline while this call is still copying. The epoch says whether that happened, and
		// unlike the cursor guards this replaces it never gates a write to a slot another thread
		// owns: on a mismatch the read simply publishes nothing.
		const epoch = Atomics.load(this.#control, EPOCH);

		// The reader's own tally is authoritative: nothing else writes CONSUMED.
		let consumed = this.#consumed;
		let read = (consumed - Atomics.load(this.#control, ANCHOR)) | 0;
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);

		// Latency skip: if buffered data exceeds LATENCY, skip ahead.
		// CAS ensures we never step backward relative to a concurrent writer advance.
		// Disabled in buffered mode, where we deliberately play through the whole buffer.
		const buffered = (write - read) | 0;
		if (!this.buffered && latency > 0 && buffered > latency) {
			const skipTo = (write - latency) | 0;
			const skip = (skipTo - read) | 0;
			if (skip > 0) {
				consumed = (consumed + skip) | 0;
				read = skipTo;
			}
		}

		const available = (write - read) | 0;
		const count = Math.min(available, output[0].length);
		if (count <= 0) {
			// A latency skip still has to be published, but only if it still means anything.
			if (Atomics.load(this.#control, EPOCH) === epoch) this.#publish(consumed);
			return 0;
		}

		// Copy samples
		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#samples[channel];
			const dst = output[channel];
			for (let i = 0; i < count; i++) {
				dst[i] = src[slot((read + i) | 0, this.capacity)];
			}
		}

		if (Atomics.load(this.#control, EPOCH) !== epoch) {
			// These samples belong to a timeline that no longer exists. The worklet takes the
			// return value as an underflow count rather than clearing the buffer it handed over,
			// so silence the written prefix or the discarded audio renders anyway.
			for (let channel = 0; channel < this.channels; channel++) output[channel].fill(0, 0, count);
			return 0;
		}

		this.#publish((consumed + count) | 0);

		return count;
	}

	/** Update the target latency in samples. */
	setLatency(samples: number): void {
		Atomics.store(this.#control, LATENCY, samples);
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping whatever is already due.
	 * Main thread only.
	 *
	 * A successor track overwrites the slots its own samples land on, but anything the previous
	 * track wrote beyond them would otherwise still play once the successor runs out.
	 */
	truncate(timestamp: Time.Micro): void {
		const target = (Math.round(Time.Second.fromMicro(timestamp) * this.rate) - this.#anchor) | 0;
		for (;;) {
			const write = Atomics.load(this.#control, WRITE);
			if (((write - target) | 0) <= 0) return; // nothing buffered past the new timeline
			// Never retreat past the playhead: those samples are already due. The worklet can still
			// advance READ past the value read here, leaving READ ahead of WRITE. That reads as an
			// empty ring (read() returns nothing, insert() trims what the worklet already played) and
			// heals on the first successor sample past the playhead, so it costs a quantum of silence
			// rather than replaying anything.
			const clamped = i32Max(target, this.#read());
			if (((write - clamped) | 0) <= 0) return;
			if (Atomics.compareExchange(this.#control, WRITE, write, clamped) === write) return;
		}
	}

	/**
	 * Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode).
	 * Main thread only. The worklet reader sees STALLED and stops until the next insert.
	 */
	reset(): void {
		this.#anchored = false;
		Atomics.store(this.#control, STALLED, 1);
		// Drain everything buffered by putting the playhead at WRITE, via ANCHOR rather than a slot
		// the reader owns.
		const write = Atomics.load(this.#control, WRITE);
		Atomics.store(this.#control, ANCHOR, (Atomics.load(this.#control, CONSUMED) - write) | 0);
	}

	/**
	 * Allocate a new ring with `newCapacity` samples and copy the unread window
	 * [READ, WRITE) plus control state into it. Used when growing capacity so
	 * we don't drop buffered audio. If `newCapacity` is smaller than the unread
	 * span, the oldest samples are truncated.
	 *
	 * Main thread only. `resize()` reads from the source `SharedRingBuffer` and
	 * writes into a freshly allocated buffer from `allocSharedRingBuffer`, so it
	 * relies on the same invariant as `insert()`: no concurrent main-thread
	 * writers. The AudioWorklet reader is tolerated via the CAS discipline used
	 * by READ/WRITE elsewhere.
	 */
	resize(newCapacity: number): SharedRingBuffer {
		const init = allocSharedRingBuffer(this.channels, newCapacity, this.rate, this.buffered);
		const dst = new SharedRingBuffer(init);
		dst.#anchored = this.#anchored;
		dst.#anchor = this.#anchor;

		// One snapshot for both. Loading CONSUMED again for the playhead would let the reader
		// advance in between, so `copyStart` would already include that delta and the handoff
		// would add it a second time.
		const consumed = Atomics.load(this.#control, CONSUMED);
		const read = (consumed - Atomics.load(this.#control, ANCHOR)) | 0;
		const write = Atomics.load(this.#control, WRITE);
		const latency = Atomics.load(this.#control, LATENCY);
		const stalled = Atomics.load(this.#control, STALLED);

		const available = (write - read) | 0;
		const copyCount = Math.max(0, Math.min(available, dst.capacity));
		const copyStart = (write - copyCount) | 0;

		for (let channel = 0; channel < this.channels; channel++) {
			const src = this.#samples[channel];
			const out = dst.#samples[channel];
			for (let i = 0; i < copyCount; i++) {
				const idx = (copyStart + i) | 0;
				out[slot(idx, dst.capacity)] = src[slot(idx, this.capacity)];
			}
		}

		// Seed the replacement so a main-thread read of it lines up before the worklet switches
		// over. The worklet then publishes its own higher tally, which is monotonic against this.
		Atomics.store(dst.#control, CONSUMED, consumed);
		Atomics.store(dst.#control, ANCHOR, (consumed - copyStart) | 0);
		Atomics.store(dst.#control, EPOCH, Atomics.load(this.#control, EPOCH));
		Atomics.store(dst.#control, WRITE, write);
		Atomics.store(dst.#control, LATENCY, latency);
		Atomics.store(dst.#control, STALLED, stalled);

		// Carry the unwrapped playhead over, rebased onto dst's READ. Fold the same `read`
		// snapshot the copy used so both sides agree on one observation; `copyStart` is at or
		// ahead of it whenever the copy dropped the oldest samples.
		dst.#position = this.#foldRead(read) + ((copyStart - read) | 0);
		dst.#lastRead = copyStart;

		return dst;
	}

	/** Record samples played. Worklet only, and the sole writer of CONSUMED. */
	#publish(consumed: number): void {
		this.#consumed = consumed;
		Atomics.store(this.#control, CONSUMED, consumed);
	}

	/**
	 * The playhead, in anchor-relative samples.
	 *
	 * Derived rather than stored, so no slot has two writers. A re-anchor moves ANCHOR while the
	 * reader moves CONSUMED, and the two loads are not atomic together, so this can be off by at
	 * most what the reader played in that window: one quantum, self-healing on the next insert.
	 * It can never be off by the length of the previous timeline, which is what a shared cursor
	 * risked every time one side rebased it.
	 */
	#read(): number {
		return (Atomics.load(this.#control, CONSUMED) - Atomics.load(this.#control, ANCHOR)) | 0;
	}

	/** Move the playhead to `position` by rebasing ANCHOR. Main thread only, and never backwards. */
	#seek(position: number): void {
		const anchor = (Atomics.load(this.#control, CONSUMED) - position) | 0;
		if (((Atomics.load(this.#control, ANCHOR) - anchor) | 0) > 0) {
			Atomics.store(this.#control, ANCHOR, anchor);
		}
	}

	/**
	 * Fold an observed READ into `#position`, so the returned offset keeps counting up across
	 * the i32 wrap. Idempotent: folding the same value twice adds a zero delta.
	 */
	#foldRead(read: number): number {
		this.#position += (read - this.#lastRead) | 0;
		this.#lastRead = read;
		return this.#position;
	}

	/** `#foldRead` against the current READ. */
	#unwrapRead(): number {
		return this.#foldRead(this.#read());
	}

	/**
	 * Current playback timestamp derived from READ position.
	 *
	 * Main thread only, and stateful: it advances the unwrapped read position, so it has to be
	 * polled rather than sampled once. See `#position`.
	 */
	get timestamp(): Time.Micro {
		return Time.Micro.fromSecond(((this.#anchor + this.#unwrapRead()) / this.rate) as Time.Second);
	}

	/** Whether the buffer is stalled (waiting to fill). */
	get stalled(): boolean {
		return Atomics.load(this.#control, STALLED) === 1;
	}

	/**
	 * Number of buffered samples (WRITE - READ).
	 *
	 * Non-atomic: WRITE and READ are loaded separately, so a concurrent
	 * writer/reader can make the two loads inconsistent. Intended for
	 * tests and diagnostics, not control-flow decisions.
	 */
	get length(): number {
		return (Atomics.load(this.#control, WRITE) - this.#read()) | 0;
	}
}
